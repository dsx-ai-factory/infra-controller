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

//! The RMS routing proxy: one `RackManager` and `RackManagerV2` front for the whole fleet.
//!
//! Every machine-a-tron instance serves the RMS mock for the racks it simulates. NICo is
//! configured with one RMS endpoint, so the gateway implements both services itself and forwards
//! each request by the rack its nodes name, as decided by [`Ownership`]:
//!
//! - Rack-scoped RPCs (`GetScaleUpFabricStatus`, `RackManagerV2.ConfigureScaleUpFabricManager`)
//!   go to the one instance owning the racks the nodes name, unchanged; nodes on two instances
//!   are refused.
//! - Node batches (device info, power, fabric service status, certificates, password rotation,
//!   factory reset, firmware and NVOS image applies) are split by rack owner, forwarded
//!   concurrently and merged back in request order. A node whose rack nobody owns is a per-node
//!   failure and is never sent to an arbitrary instance; an instance that fails a call fails only
//!   its own nodes, and a request every instance refuses as `INVALID_ARGUMENT` is refused with
//!   that status.
//! - `ListFirmwareObjects` names no node: every instance is asked and the catalogues are merged
//!   by object id.
//! - Job ids in every response are replaced by gateway ids from [`JobMap`], and the job status
//!   RPCs resolve their id through the same map. A batch split across instances gets one
//!   aggregate parent whose status is the worst, least advanced of the per-instance parents,
//!   with no rack or node of its own. An id the map does not know is answered as the RMS mock
//!   answers an id it never issued:
//!   complete by `GetJobStatus` and `GetConfigureSwitchCertificateJobStatus`,
//!   `RETURN_CODE_FAILURE` with `job <id> not found` by `GetFirmwareJobStatus` and
//!   `GetSwitchSystemImageJobStatus`.
//! - `GetVersion` is answered locally. Every RPC the RMS mock does not implement is
//!   `UNIMPLEMENTED` here as well.
//!
//! Until the source list is bound, every forwarded RPC answers `UNAVAILABLE`; rack-scoped RPCs
//! and node batches also wait for ownership to be ready. Every forwarded call is bounded by
//! `rms.request_timeout`, connection setup included.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Router;
use futures::future::join_all;
use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
use librms::protos::rack_manager::rack_manager_server::{RackManager, RackManagerServer};
use librms::protos::rack_manager_v2::rack_manager_v2_client::RackManagerV2Client;
use librms::protos::rack_manager_v2::rack_manager_v2_server::{RackManagerV2, RackManagerV2Server};
use librms::protos::{rack_manager as rms, rack_manager_v2 as rms_v2};
use tonic::server::NamedService;
use tonic::transport::Channel;
use tonic::{Code, Request, Response, Status};

use crate::config::{RmsConfig, SourceClientConfig};
use crate::ownership::{Owner, Ownership, SourceId};
use crate::rms_client::BackendConnector;
use crate::rms_jobs::{BackendJob, JobMap, JobPhase, JobRecord};
use crate::sources::SourceList;

/// What `GetVersion` reports; `librms` probes new connections with it.
pub const RMS_VERSION: &str = concat!("mat-protocol-gateway/", env!("CARGO_PKG_VERSION"));

/// The stubs for one machine-a-tron instance.
#[derive(Clone)]
struct Backend {
    source: SourceId,
    v1: RackManagerClient<Channel>,
    v2: RackManagerV2Client<Channel>,
}

type Backends = Arc<HashMap<SourceId, Backend>>;

/// Routes RMS requests to the instances of the bound source set.
pub(crate) struct RmsProxy {
    ownership: Arc<dyn Ownership>,
    connector: BackendConnector,
    request_timeout: Duration,
    backends: RwLock<Option<Backends>>,
    jobs: JobMap,
}

impl RmsProxy {
    /// A proxy with no instances yet; [`Self::bind_sources`] adds them once the controller's list
    /// is known. The CA file named by `sources` is read here.
    pub(crate) fn new(
        ownership: Arc<dyn Ownership>,
        sources: &SourceClientConfig,
        rms: &RmsConfig,
    ) -> eyre::Result<Self> {
        Ok(Self {
            ownership,
            connector: BackendConnector::new(sources, rms)?,
            request_timeout: rms.request_timeout,
            backends: RwLock::new(None),
            jobs: JobMap::new(),
        })
    }

    /// Points the proxy at the instances of `list`. Connections are opened by the first forwarded
    /// request, so an unreachable instance is reported per request rather than here.
    pub(crate) fn bind_sources(&self, list: &SourceList) -> eyre::Result<()> {
        let mut backends = HashMap::with_capacity(list.sources.len());
        for source in &list.sources {
            let channel = self.connector.channel(&source.base_url)?;
            let id = SourceId::from(source);
            backends.insert(
                id.clone(),
                Backend {
                    source: id,
                    v1: RackManagerClient::new(channel.clone()),
                    v2: RackManagerV2Client::new(channel),
                },
            );
        }
        tracing::info!(sources = ?list.names(), "Routing RMS requests to the controller source set");
        *self
            .backends
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(backends));
        Ok(())
    }

    fn backends(&self) -> Result<Backends, Status> {
        self.backends
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or_else(|| {
                Status::unavailable("the gateway is waiting for the controller source list")
            })
    }

    fn backend(&self, source: &SourceId) -> Result<Backend, Status> {
        self.backends()?.get(source).cloned().ok_or_else(|| {
            Status::failed_precondition(format!(
                "machine-a-tron {source} is not in the gateway source set"
            ))
        })
    }

    fn ensure_ownership_ready(&self) -> Result<(), Status> {
        if self.ownership.is_ready() {
            Ok(())
        } else {
            Err(Status::unavailable(
                "the gateway has not learned which machine-a-tron instance owns each rack yet",
            ))
        }
    }

    /// Awaits one forwarded call for at most `rms.request_timeout`, connection setup included.
    async fn bounded<T>(&self, call: impl Future<Output = Result<T, Status>>) -> Result<T, Status> {
        tokio::time::timeout(self.request_timeout, call)
            .await
            .unwrap_or_else(|_elapsed| {
                Err(Status::deadline_exceeded(format!(
                    "no answer within {:?}",
                    self.request_timeout
                )))
            })
    }

    /// Groups the nodes of a batch by the instance owning their rack, in order of first
    /// appearance; nodes without a routable owner are kept with the reason.
    fn split(&self, nodes: &[rms::NodeInfo]) -> Result<Split, Status> {
        self.ensure_ownership_ready()?;
        let backends = self.backends()?;
        let mut groups: Vec<Group> = Vec::new();
        let mut unowned = Vec::new();
        for (index, node) in nodes.iter().enumerate() {
            let owner = match self.rack_owner_of(node) {
                Ok(owner) => owner,
                Err(reason) => {
                    unowned.push((index, reason.to_string()));
                    continue;
                }
            };
            let Some(backend) = backends.get(&owner) else {
                unowned.push((
                    index,
                    format!("machine-a-tron {owner} owns rack {:?} but is not in the gateway source set", node.rack_id),
                ));
                continue;
            };
            match groups
                .iter_mut()
                .find(|group| group.backend.source == owner)
            {
                Some(group) => group.indices.push(index),
                None => groups.push(Group {
                    backend: backend.clone(),
                    indices: vec![index],
                }),
            }
        }
        Ok(Split { groups, unowned })
    }

    /// The instance simulating the rack `node` names, or why there is none.
    fn rack_owner_of(&self, node: &rms::NodeInfo) -> Result<SourceId, Unroutable> {
        if node.rack_id.trim().is_empty() {
            return Err(Unroutable::NoRack {
                node_id: node.node_id.clone(),
            });
        }
        let rack_id = node.rack_id.clone();
        match self.ownership.owner_of_rack(&rack_id) {
            Owner::Source(source) => Ok(source),
            Owner::Dropped(source) => Err(Unroutable::Dropped { source, rack_id }),
            Owner::Ambiguous(sources) => Err(Unroutable::Ambiguous { sources, rack_id }),
            Owner::Unknown => Err(Unroutable::Unknown { rack_id }),
        }
    }

    /// The single instance a rack-scoped request goes to. Every node must name a rack of the
    /// same owner: the RMS operations routed this way act on one instance, so a request spanning
    /// instances is a caller error. A node whose rack cannot be routed keeps its own status
    /// wherever it sits in the request: a dropped owner is `UNAVAILABLE`, an unknown rack
    /// `NOT_FOUND`.
    fn rack_owner(&self, nodes: &[rms::NodeInfo]) -> Result<Backend, Status> {
        self.ensure_ownership_ready()?;
        let Some(first) = nodes.first() else {
            return Err(Status::invalid_argument("the request names no nodes"));
        };
        let owner = self.rack_owner_of(first)?;
        for node in &nodes[1..] {
            if self.rack_owner_of(node)? != owner {
                return Err(Status::invalid_argument(format!(
                    "the nodes span rack {:?} on machine-a-tron {owner} and rack {:?}; a rack-scoped request must name racks of one instance",
                    first.rack_id, node.rack_id
                )));
            }
        }
        self.backend(&owner)
    }

    /// Forwards one batch to every owning instance at once. `call` performs the RPC on an
    /// instance and the subset of `nodes` it owns; a failing instance is kept as a failed part so
    /// the merge can report its nodes individually, except that a request every instance refuses
    /// as `INVALID_ARGUMENT` is refused the same way.
    async fn fan_out<T, F, Fut>(
        &self,
        nodes: &[rms::NodeInfo],
        call: F,
    ) -> Result<FanOut<T>, Status>
    where
        F: Fn(Backend, Vec<rms::NodeInfo>) -> Fut,
        Fut: Future<Output = Result<T, Status>>,
    {
        let split = self.split(nodes)?;
        let calls = split.groups.into_iter().map(|group| {
            let subset = group
                .indices
                .iter()
                .map(|&index| nodes[index].clone())
                .collect();
            let source = group.backend.source.clone();
            let future = call(group.backend, subset);
            async move {
                Part {
                    source,
                    indices: group.indices,
                    outcome: self.bounded(future).await,
                }
            }
        });
        let fan = FanOut {
            parts: join_all(calls).await,
            unowned: split.unowned,
        };
        match refused_by_every_instance(&fan) {
            Some(status) => Err(status),
            None => Ok(fan),
        }
    }

    /// Merges the per-instance batch envelopes back into one, in request order.
    ///
    /// The caller checks the batch status, the per-node results and `stats.failed_nodes`
    /// independently, so all three are derived from the same merged results. The batch job id is
    /// the gateway id for whatever parents the instances issued.
    fn merge_batches<T>(
        &self,
        nodes: &[rms::NodeInfo],
        fan: &FanOut<T>,
        envelope: impl Fn(&T) -> Option<&rms::NodeBatchResponse>,
    ) -> rms::NodeBatchResponse {
        let mut results: Vec<Option<rms::NodeOperationResult>> = vec![None; nodes.len()];
        let mut messages = Vec::new();
        let mut parents = Vec::new();
        for part in &fan.parts {
            let batch = match &part.outcome {
                Ok(response) => envelope(response),
                Err(status) => {
                    let reason = backend_failure(&part.source, status);
                    messages.push(reason.clone());
                    for &index in &part.indices {
                        results[index] = Some(failed(&nodes[index].node_id, reason.clone()));
                    }
                    continue;
                }
            };
            let Some(batch) = batch else {
                let reason = format!("machine-a-tron {} returned no batch response", part.source);
                messages.push(reason.clone());
                for &index in &part.indices {
                    results[index] = Some(failed(&nodes[index].node_id, reason.clone()));
                }
                continue;
            };
            let answers = correlate(nodes, &part.indices, &batch.node_results, |result| {
                &result.node_id
            });
            for (&index, answer) in part.indices.iter().zip(answers) {
                results[index] = Some(answer.cloned().unwrap_or_else(|| {
                    failed(
                        &nodes[index].node_id,
                        format!(
                            "machine-a-tron {} returned no result for this node",
                            part.source
                        ),
                    )
                }));
            }
            if !batch.message.is_empty() {
                messages.push(format!("machine-a-tron {}: {}", part.source, batch.message));
            }
            if !batch.job_id.is_empty() {
                parents.push(BackendJob {
                    source: part.source.clone(),
                    job_id: batch.job_id.clone(),
                });
            }
        }
        for (index, reason) in &fan.unowned {
            results[*index] = Some(failed(&nodes[*index].node_id, reason.clone()));
        }

        let node_results: Vec<rms::NodeOperationResult> = results.into_iter().flatten().collect();
        let total = node_results.len() as u32;
        let failed_nodes = node_results
            .iter()
            .filter(|result| result.status != rms::ReturnCode::Success as i32)
            .count() as u32;
        if failed_nodes > 0 && messages.is_empty() {
            messages.push(format!("{failed_nodes} of {total} nodes failed"));
        }
        rms::NodeBatchResponse {
            status: return_code(failed_nodes == 0),
            message: messages.join("; "),
            node_results,
            job_id: self.jobs.batch_id(parents),
            stats: Some(rms::NodeOperationStats {
                total_nodes: total,
                successful_nodes: total - failed_nodes,
                failed_nodes,
            }),
        }
    }

    /// The per-node jobs of a batch as `(node id, gateway job id)` in request order; `job` reads
    /// the node and instance job ids of one entry.
    fn node_jobs<'a, T, J: 'a>(
        &self,
        nodes: &[rms::NodeInfo],
        fan: &'a FanOut<T>,
        jobs: impl Fn(&'a T) -> &'a [J],
        job: impl Fn(&J) -> (&str, &str),
    ) -> Vec<(String, String)> {
        merge_entries(nodes, fan, jobs, |entry| job(entry).0)
            .into_iter()
            .flatten()
            .map(|(source, entry)| {
                let (node_id, job_id) = job(entry);
                (node_id.to_owned(), self.jobs.intern(source, job_id))
            })
            .collect()
    }

    /// `None` for an id this process does not know: the gateway forgets its table on restart and
    /// after the retention, and NICo persists job ids across both, so each status RPC answers
    /// such an id the way the RMS mock answers an id it never issued.
    fn resolve_job(&self, gateway_job_id: &str) -> Result<Option<JobRecord>, Status> {
        if gateway_job_id.trim().is_empty() {
            return Err(Status::invalid_argument("job_id is required"));
        }
        let record = self.jobs.resolve(gateway_job_id);
        if record.is_none() {
            tracing::warn!(
                job_id = gateway_job_id,
                "Status poll names a job id the gateway does not know"
            );
        }
        Ok(record)
    }

    /// Rewrites the ids of one instance's `JobStatus` to gateway ids. A per-instance parent that
    /// was folded into an aggregate reports the aggregate as its parent.
    fn translate_job_status(
        &self,
        source: &SourceId,
        mut status: rms::JobStatus,
    ) -> rms::JobStatus {
        let aggregate = self.jobs.aggregate_of(&BackendJob {
            source: source.clone(),
            job_id: status.job_id.clone(),
        });
        status.job_id = self.jobs.intern(source, &status.job_id);
        status.parent_job_id = aggregate.or_else(|| {
            status
                .parent_job_id
                .as_deref()
                .filter(|parent| !parent.is_empty())
                .map(|parent| self.jobs.intern(source, parent))
        });
        status.child_job_ids = status
            .child_job_ids
            .iter()
            .map(|child| self.jobs.intern(source, child))
            .collect();
        status
    }

    /// `message` as the caller knows the job: an instance that has forgotten `job` names its own
    /// id in `job <id> not found`, the one instance message carrying a job id.
    fn rewrite_not_found(&self, job: &BackendJob, message: &str) -> String {
        if message == job_not_found(&job.job_id) {
            job_not_found(&self.jobs.intern(&job.source, &job.job_id))
        } else {
            message.to_owned()
        }
    }

    /// Polls the per-instance parents of an aggregate, failing on the first instance error.
    async fn poll_parts<T, F, Fut>(&self, parts: &[BackendJob], call: F) -> Result<Vec<T>, Status>
    where
        F: Fn(Backend, String) -> Fut,
        Fut: Future<Output = Result<T, Status>>,
    {
        let call = &call;
        let polls = parts.iter().map(|part| {
            let backend = self.backend(&part.source);
            let job_id = part.job_id.clone();
            async move {
                let backend = backend?;
                let source = backend.source.clone();
                self.bounded(call(backend, job_id))
                    .await
                    .map_err(|status| backend_error(&source, status))
            }
        });
        join_all(polls).await.into_iter().collect()
    }
}

/// Both RMS gRPC services, served by `proxy`, on the paths `librms` clients call.
///
/// Mounted as plain `tower` services like `rms_mock::router` does, so the UFM router's fallback
/// survives the merge, with the paths taken from each service's `NamedService::NAME`.
pub(crate) fn router(proxy: Arc<RmsProxy>) -> Router {
    let v1_path = format!(
        "/{}/{{*rpc}}",
        <RackManagerServer<RmsProxy> as NamedService>::NAME
    );
    let v2_path = format!(
        "/{}/{{*rpc}}",
        <RackManagerV2Server<RmsProxy> as NamedService>::NAME
    );
    Router::new()
        .route_service(&v1_path, RackManagerServer::from_arc(proxy.clone()))
        .route_service(&v2_path, RackManagerV2Server::from_arc(proxy))
}

/// Why a node's rack has no instance to route to; the gRPC code follows from the variant.
#[derive(Debug, Eq, PartialEq)]
enum Unroutable {
    NoRack {
        node_id: String,
    },
    Dropped {
        source: SourceId,
        rack_id: String,
    },
    Ambiguous {
        sources: Vec<SourceId>,
        rack_id: String,
    },
    Unknown {
        rack_id: String,
    },
}

impl Unroutable {
    fn code(&self) -> Code {
        match self {
            Self::NoRack { .. } | Self::Ambiguous { .. } => Code::InvalidArgument,
            Self::Dropped { .. } => Code::Unavailable,
            Self::Unknown { .. } => Code::NotFound,
        }
    }
}

impl fmt::Display for Unroutable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRack { node_id } => write!(formatter, "node {node_id:?} names no rack"),
            Self::Dropped { source, rack_id } => write!(
                formatter,
                "machine-a-tron {source} owns rack {rack_id:?} but has not answered its status polls for ownership.stale_after; retry once it answers"
            ),
            Self::Ambiguous { sources, rack_id } => write!(
                formatter,
                "rack {rack_id:?} is reported by machine-a-tron instances {}",
                sources
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
            Self::Unknown { rack_id } => {
                write!(
                    formatter,
                    "no machine-a-tron instance owns rack {rack_id:?}"
                )
            }
        }
    }
}

impl From<Unroutable> for Status {
    fn from(reason: Unroutable) -> Self {
        Status::new(reason.code(), reason.to_string())
    }
}

/// A batch's nodes grouped by owning instance.
struct Group {
    backend: Backend,
    indices: Vec<usize>,
}

struct Split {
    groups: Vec<Group>,
    /// Request indices with no routable owner, and why.
    unowned: Vec<(usize, String)>,
}

/// One instance's share of a fanned-out batch.
struct Part<T> {
    source: SourceId,
    indices: Vec<usize>,
    outcome: Result<T, Status>,
}

struct FanOut<T> {
    parts: Vec<Part<T>>,
    unowned: Vec<(usize, String)>,
}

/// Pairs the request entries of one part (`indices` into `nodes`) with the entries an instance
/// answered for them.
///
/// When the part's request node ids are distinct and non-empty the answers are matched by node
/// id, which tolerates an instance answering in another order. NICo can send the same node id
/// twice, or none at all, and then a match by id would hand two nodes the same answer; in that
/// case the answers are taken in request order, which is how the mock returns them, and a count
/// mismatch leaves the part's nodes unmatched rather than guessing.
fn correlate<'a, E>(
    nodes: &[rms::NodeInfo],
    indices: &[usize],
    entries: &'a [E],
    node_id: impl Fn(&E) -> &str,
) -> Vec<Option<&'a E>> {
    let requested: Vec<&str> = indices
        .iter()
        .map(|&index| nodes[index].node_id.as_str())
        .collect();
    let distinct = requested.iter().all(|id| !id.is_empty())
        && requested.iter().collect::<HashSet<_>>().len() == requested.len();
    if distinct {
        let by_id: HashMap<&str, &E> = entries
            .iter()
            .map(|entry| (node_id(entry), entry))
            .collect();
        requested.iter().map(|id| by_id.get(id).copied()).collect()
    } else if entries.len() == indices.len() {
        entries.iter().map(Some).collect()
    } else {
        vec![None; indices.len()]
    }
}

/// The per-node entries of a fanned-out batch in request order, each with the instance that
/// answered it; `None` for a node whose instance failed or answered nothing for it. `entries`
/// reads the list from one instance's response.
fn merge_entries<'a, T, E>(
    nodes: &[rms::NodeInfo],
    fan: &'a FanOut<T>,
    entries: impl Fn(&'a T) -> &'a [E],
    node_id: impl Fn(&E) -> &str,
) -> Vec<Option<(&'a SourceId, &'a E)>> {
    let mut merged = vec![None; nodes.len()];
    for part in &fan.parts {
        let Ok(response) = &part.outcome else {
            continue;
        };
        let answers = correlate(nodes, &part.indices, entries(response), &node_id);
        for (&index, answer) in part.indices.iter().zip(answers) {
            merged[index] = answer.map(|entry| (&part.source, entry));
        }
    }
    merged
}

/// The response of the first instance that answered, in part order.
fn first_answer<T>(fan: &FanOut<T>) -> Option<&T> {
    fan.parts.iter().find_map(|part| part.outcome.as_ref().ok())
}

/// The refusal to answer with when every instance refused the request as `INVALID_ARGUMENT`,
/// naming the first: that is the caller's error, not a per-node failure. `None` when any part
/// answered or failed otherwise, and when no instance was asked.
fn refused_by_every_instance<T>(fan: &FanOut<T>) -> Option<Status> {
    let mut refusals = fan.parts.iter().map(|part| match &part.outcome {
        Err(status) if status.code() == Code::InvalidArgument => Some((&part.source, status)),
        _ => None,
    });
    let (source, status) = refusals.next().flatten()?;
    refusals
        .all(|refusal| refusal.is_some())
        .then(|| backend_error(source, status.clone()))
}

/// One catalogue from several, in the order given; an object id seen again is the same object
/// and is kept once.
fn union_by_id(catalogues: Vec<Vec<rms::FirmwareObject>>) -> Vec<rms::FirmwareObject> {
    let mut seen = HashSet::new();
    catalogues
        .into_iter()
        .flatten()
        .filter(|object| seen.insert(object.id.clone()))
        .collect()
}

fn requested_nodes(nodes: &Option<rms::NodeSet>) -> Vec<rms::NodeInfo> {
    nodes
        .as_ref()
        .map(|set| set.nodes.clone())
        .unwrap_or_default()
}

fn return_code(success: bool) -> i32 {
    if success {
        rms::ReturnCode::Success as i32
    } else {
        rms::ReturnCode::Failure as i32
    }
}

/// Whether every part of an aggregate answered `RETURN_CODE_SUCCESS`.
fn all_success(statuses: impl IntoIterator<Item = i32>) -> bool {
    statuses
        .into_iter()
        .all(|status| status == rms::ReturnCode::Success as i32)
}

fn failed(node_id: &str, error_message: String) -> rms::NodeOperationResult {
    rms::NodeOperationResult {
        node_id: node_id.to_owned(),
        status: rms::ReturnCode::Failure as i32,
        error_message,
    }
}

fn backend_failure(source: &SourceId, status: &Status) -> String {
    format!(
        "machine-a-tron {source}: {} ({:?})",
        status.message(),
        status.code()
    )
}

fn backend_error(source: &SourceId, status: Status) -> Status {
    Status::new(
        status.code(),
        format!("machine-a-tron {source}: {}", status.message()),
    )
}

/// The `error_message` the firmware and NVOS status RPCs carry for a job never issued.
fn job_not_found(job_id: &str) -> String {
    format!("job {job_id} not found")
}

/// The non-empty error messages of an aggregate's parts, each attributed to its instance.
fn join_errors<'a, M: AsRef<str>>(errors: impl IntoIterator<Item = (&'a SourceId, M)>) -> String {
    errors
        .into_iter()
        .filter(|(_, message)| !message.as_ref().is_empty())
        .map(|(source, message)| format!("machine-a-tron {source}: {}", message.as_ref()))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Builds the whole `RackManager` impl.
///
/// Every generated method must exist, so the ones the gateway does not route are generated to
/// return `UNIMPLEMENTED`; a `librms` bump that adds an RPC fails to compile here. The macro
/// emits the `async_trait` attribute itself: attribute macros expand before the function-like
/// macros in their body, so a nested invocation would leave plain `async fn`s the trait cannot
/// accept.
macro_rules! rack_manager_impl {
    (
        routed { $($routed:tt)* }
        unimplemented { $($method:ident($request:ident) -> $response:ident,)* }
    ) => {
        #[tonic::async_trait]
        impl RackManager for RmsProxy {
            $($routed)*

            $(
                /// Not implemented by the RMS mock, so not routed by the gateway.
                async fn $method(
                    &self,
                    _request: Request<rms::$request>,
                ) -> Result<Response<rms::$response>, Status> {
                    Err(Status::unimplemented(concat!(
                        "the machine-a-tron protocol gateway does not route ",
                        stringify!($method),
                    )))
                }
            )*
        }
    };
}

rack_manager_impl! {
    routed {
        /// Answered locally: this is the connection probe, and it must not depend on any
        /// instance being reachable.
        async fn get_version(
            &self,
            _request: Request<rms::GetVersionRequest>,
        ) -> Result<Response<rms::GetVersionResponse>, Status> {
            Ok(Response::new(rms::GetVersionResponse {
                version: RMS_VERSION.to_owned(),
            }))
        }

        /// Rack-scoped: forwarded unchanged to the rack's owner.
        async fn get_scale_up_fabric_status(
            &self,
            request: Request<rms::GetScaleUpFabricStatusRequest>,
        ) -> Result<Response<rms::GetScaleUpFabricStatusResponse>, Status> {
            let request = request.into_inner();
            let mut backend = self.rack_owner(&requested_nodes(&request.nodes))?;
            self.bounded(backend.v1.get_scale_up_fabric_status(request))
                .await
                .map_err(|status| backend_error(&backend.source, status))
        }

        /// Node batch; device details are only returned for nodes an instance knows.
        async fn batch_get_node_device_info(
            &self,
            request: Request<rms::BatchGetNodeDeviceInfoRequest>,
        ) -> Result<Response<rms::BatchGetNodeDeviceInfoResponse>, Status> {
            let request = request.into_inner();
            let nodes = requested_nodes(&request.nodes);
            let fan = self
                .fan_out(&nodes, |mut backend, subset| {
                    let mut part = request.clone();
                    part.nodes = Some(rms::NodeSet { nodes: subset });
                    async move {
                        backend
                            .v1
                            .batch_get_node_device_info(part)
                            .await
                            .map(Response::into_inner)
                    }
                })
                .await?;

            let mut messages = Vec::new();
            let mut successful = 0u32;
            for part in &fan.parts {
                match &part.outcome {
                    Err(status) => messages.push(backend_failure(&part.source, status)),
                    Ok(response) => {
                        successful += response.stats.as_ref().map_or_else(
                            || response.node_device_details.len() as u32,
                            |stats| stats.successful_nodes,
                        );
                        if !response.message.is_empty() {
                            messages.push(format!(
                                "machine-a-tron {}: {}",
                                part.source, response.message
                            ));
                        }
                    }
                }
            }
            for (index, reason) in &fan.unowned {
                messages.push(format!("node {}: {reason}", nodes[*index].node_id));
            }
            let total = nodes.len() as u32;
            let successful = successful.min(total);
            let failed_nodes = total - successful;
            if failed_nodes > 0 && messages.is_empty() {
                messages.push(format!("{failed_nodes} of {total} nodes failed"));
            }
            let details = merge_entries(
                &nodes,
                &fan,
                |response| response.node_device_details.as_slice(),
                |detail| &detail.node_id,
            );
            Ok(Response::new(rms::BatchGetNodeDeviceInfoResponse {
                status: return_code(failed_nodes == 0),
                message: messages.join("; "),
                node_device_details: details
                    .into_iter()
                    .flatten()
                    .map(|(_, detail)| detail.clone())
                    .collect(),
                stats: Some(rms::NodeOperationStats {
                    total_nodes: total,
                    successful_nodes: successful,
                    failed_nodes,
                }),
            }))
        }

        /// Node batch; a power state is only returned for nodes an instance could read.
        async fn batch_get_power_state(
            &self,
            request: Request<rms::BatchGetPowerStateRequest>,
        ) -> Result<Response<rms::BatchGetPowerStateResponse>, Status> {
            let request = request.into_inner();
            let nodes = requested_nodes(&request.nodes);
            let fan = self
                .fan_out(&nodes, |mut backend, subset| {
                    let mut part = request.clone();
                    part.nodes = Some(rms::NodeSet { nodes: subset });
                    async move {
                        backend
                            .v1
                            .batch_get_power_state(part)
                            .await
                            .map(Response::into_inner)
                    }
                })
                .await?;

            let states = merge_entries(
                &nodes,
                &fan,
                |response| response.node_power_states.as_slice(),
                |state| &state.node_id,
            );
            Ok(Response::new(rms::BatchGetPowerStateResponse {
                response: Some(self.merge_batches(&nodes, &fan, |r| r.response.as_ref())),
                node_power_states: states
                    .into_iter()
                    .flatten()
                    .map(|(_, state)| state.clone())
                    .collect(),
            }))
        }

        /// Node batch; the operation is forwarded as is and validated by each instance.
        async fn batch_set_power_state(
            &self,
            request: Request<rms::BatchSetPowerStateRequest>,
        ) -> Result<Response<rms::BatchSetPowerStateResponse>, Status> {
            let request = request.into_inner();
            let nodes = requested_nodes(&request.nodes);
            let fan = self
                .fan_out(&nodes, |mut backend, subset| {
                    let mut part = request.clone();
                    part.nodes = Some(rms::NodeSet { nodes: subset });
                    async move {
                        backend
                            .v1
                            .batch_set_power_state(part)
                            .await
                            .map(Response::into_inner)
                    }
                })
                .await?;
            Ok(Response::new(rms::BatchSetPowerStateResponse {
                response: Some(self.merge_batches(&nodes, &fan, |r| r.response.as_ref())),
            }))
        }

        /// Node batch; the per-switch map is the union of the instances' maps, and a node nobody
        /// answered for gets an entry carrying the reason.
        async fn batch_get_scale_up_fabric_service_status(
            &self,
            request: Request<rms::BatchGetScaleUpFabricServiceStatusRequest>,
        ) -> Result<Response<rms::BatchGetScaleUpFabricServiceStatusResponse>, Status> {
            let request = request.into_inner();
            let nodes = requested_nodes(&request.nodes);
            let fan = self
                .fan_out(&nodes, |mut backend, subset| {
                    let mut part = request.clone();
                    part.nodes = Some(rms::NodeSet { nodes: subset });
                    async move {
                        backend
                            .v1
                            .batch_get_scale_up_fabric_service_status(part)
                            .await
                            .map(Response::into_inner)
                    }
                })
                .await?;

            let error_entry = |error_message: String| rms::ScaleUpFabricServiceStatusEntry {
                status_json: String::new(),
                error_message,
            };
            let mut service_statuses = HashMap::with_capacity(nodes.len());
            let mut all_succeeded = true;
            for part in &fan.parts {
                match &part.outcome {
                    Err(status) => {
                        all_succeeded = false;
                        let reason = backend_failure(&part.source, status);
                        for &index in &part.indices {
                            service_statuses
                                .insert(nodes[index].node_id.clone(), error_entry(reason.clone()));
                        }
                    }
                    Ok(response) => {
                        all_succeeded &= response.status == rms::ReturnCode::Success as i32;
                        for &index in &part.indices {
                            let node_id = &nodes[index].node_id;
                            let entry = response.service_statuses.get(node_id).cloned().unwrap_or_else(
                                || {
                                    error_entry(format!(
                                        "machine-a-tron {} returned no status for this node",
                                        part.source
                                    ))
                                },
                            );
                            service_statuses.insert(node_id.clone(), entry);
                        }
                    }
                }
            }
            for (index, reason) in &fan.unowned {
                service_statuses.insert(nodes[*index].node_id.clone(), error_entry(reason.clone()));
            }
            let total = nodes.len() as u32;
            let failed_nodes = service_statuses
                .values()
                .filter(|entry| !entry.error_message.is_empty())
                .count() as u32;
            Ok(Response::new(rms::BatchGetScaleUpFabricServiceStatusResponse {
                status: return_code(all_succeeded && failed_nodes == 0),
                service_statuses,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: total,
                    successful_nodes: total.saturating_sub(failed_nodes),
                    failed_nodes,
                }),
            }))
        }

        /// Node batch returning a per-switch job each; the batch job id is the gateway parent.
        async fn configure_switch_certificate(
            &self,
            request: Request<rms::ConfigureSwitchCertificateRequest>,
        ) -> Result<Response<rms::ConfigureSwitchCertificateResponse>, Status> {
            let request = request.into_inner();
            let nodes = requested_nodes(&request.nodes);
            let fan = self
                .fan_out(&nodes, |mut backend, subset| {
                    let mut part = request.clone();
                    part.nodes = Some(rms::NodeSet { nodes: subset });
                    async move {
                        backend
                            .v1
                            .configure_switch_certificate(part)
                            .await
                            .map(Response::into_inner)
                    }
                })
                .await?;

            let jobs = self.node_jobs(
                &nodes,
                &fan,
                |response| response.jobs.as_slice(),
                |job| (&job.node_id, &job.job_id),
            );
            Ok(Response::new(rms::ConfigureSwitchCertificateResponse {
                response: Some(self.merge_batches(&nodes, &fan, |r| r.response.as_ref())),
                jobs: jobs
                    .into_iter()
                    .map(|(node_id, job_id)| rms::ConfigureSwitchCertificateJobInfo { node_id, job_id })
                    .collect(),
            }))
        }

        /// Node batch whose batch job id is the gateway parent, polled through `GetJobStatus`.
        async fn update_switch_system_password(
            &self,
            request: Request<rms::UpdateSwitchSystemPasswordRequest>,
        ) -> Result<Response<rms::UpdateSwitchSystemPasswordResponse>, Status> {
            let request = request.into_inner();
            let nodes = requested_nodes(&request.nodes);
            let fan = self
                .fan_out(&nodes, |mut backend, subset| {
                    let mut part = request.clone();
                    part.nodes = Some(rms::NodeSet { nodes: subset });
                    async move {
                        backend
                            .v1
                            .update_switch_system_password(part)
                            .await
                            .map(Response::into_inner)
                    }
                })
                .await?;
            Ok(Response::new(rms::UpdateSwitchSystemPasswordResponse {
                response: Some(self.merge_batches(&nodes, &fan, |r| r.response.as_ref())),
            }))
        }

        /// Node batch whose batch job id is the gateway parent, polled through `GetJobStatus`.
        async fn batch_reset_switch_factory_default(
            &self,
            request: Request<rms::BatchResetSwitchFactoryDefaultRequest>,
        ) -> Result<Response<rms::BatchResetSwitchFactoryDefaultResponse>, Status> {
            let request = request.into_inner();
            let nodes = requested_nodes(&request.nodes);
            let fan = self
                .fan_out(&nodes, |mut backend, subset| {
                    let mut part = request.clone();
                    part.nodes = Some(rms::NodeSet { nodes: subset });
                    async move {
                        backend
                            .v1
                            .batch_reset_switch_factory_default(part)
                            .await
                            .map(Response::into_inner)
                    }
                })
                .await?;
            Ok(Response::new(rms::BatchResetSwitchFactoryDefaultResponse {
                response: Some(self.merge_batches(&nodes, &fan, |r| r.response.as_ref())),
            }))
        }

        /// Names no node, so every bound instance is asked and the catalogues are merged by
        /// object id in first-seen order over the instances sorted by name. The instances render
        /// one chart-wide catalogue, so the union is normally each instance's answer; an instance
        /// that fails is left out, and the call is `UNAVAILABLE` only when none answered.
        async fn list_firmware_objects(
            &self,
            request: Request<rms::ListFirmwareObjectsRequest>,
        ) -> Result<Response<rms::ListFirmwareObjectsResponse>, Status> {
            let request = request.into_inner();
            let backends = self.backends()?;
            let mut instances: Vec<&Backend> = backends.values().collect();
            instances.sort_by(|a, b| a.source.cmp(&b.source));
            let calls = instances.into_iter().map(|backend| {
                let mut v1 = backend.v1.clone();
                let request = request.clone();
                async move {
                    let outcome = self.bounded(v1.list_firmware_objects(request)).await;
                    (&backend.source, outcome.map(Response::into_inner))
                }
            });

            let mut catalogues = Vec::new();
            let mut failures = Vec::new();
            for (source, outcome) in join_all(calls).await {
                match outcome {
                    Ok(response) => catalogues.push(response.objects),
                    Err(status) => {
                        tracing::debug!(
                            %source,
                            grpc_status_code = ?status.code(),
                            error = status.message(),
                            "Leaving an instance out of the firmware catalogue"
                        );
                        failures.push(backend_failure(source, &status));
                    }
                }
            }
            if catalogues.is_empty() {
                return Err(Status::unavailable(format!(
                    "no machine-a-tron instance answered: {}",
                    failures.join("; ")
                )));
            }
            Ok(Response::new(rms::ListFirmwareObjectsResponse {
                objects: union_by_id(catalogues),
            }))
        }

        /// Node batch with a parent job and a job per node, all gateway ids. `object_id` comes
        /// from the first instance that answered; every instance renders the same chart-wide
        /// catalogue.
        async fn apply_firmware_object(
            &self,
            request: Request<rms::ApplyFirmwareObjectRequest>,
        ) -> Result<Response<rms::ApplyFirmwareObjectResponse>, Status> {
            let request = request.into_inner();
            let nodes = requested_nodes(&request.nodes);
            let fan = self
                .fan_out(&nodes, |mut backend, subset| {
                    let mut part = request.clone();
                    part.nodes = Some(rms::NodeSet { nodes: subset });
                    async move {
                        backend
                            .v1
                            .apply_firmware_object(part)
                            .await
                            .map(Response::into_inner)
                    }
                })
                .await?;

            let jobs = self.node_jobs(
                &nodes,
                &fan,
                |response| response.jobs.as_slice(),
                |job| (&job.node_id, &job.job_id),
            );
            Ok(Response::new(rms::ApplyFirmwareObjectResponse {
                response: Some(self.merge_batches(&nodes, &fan, |r| r.response.as_ref())),
                object_id: first_answer(&fan).map(|r| r.object_id.clone()).unwrap_or_default(),
                jobs: jobs
                    .into_iter()
                    .map(|(node_id, job_id)| rms::NodeFirmwareJobInfo { node_id, job_id })
                    .collect(),
            }))
        }

        /// Resolves the gateway job id like [`RackManager::get_job_status`]. An unknown id is
        /// `RETURN_CODE_FAILURE` with `job <id> not found`, as the RMS mock answers this RPC for
        /// an id it never issued.
        async fn get_firmware_job_status(
            &self,
            request: Request<rms::GetFirmwareJobStatusRequest>,
        ) -> Result<Response<rms::GetFirmwareJobStatusResponse>, Status> {
            let gateway_job_id = request.into_inner().job_id;
            let poll = |mut backend: Backend, job_id: String| async move {
                backend
                    .v1
                    .get_firmware_job_status(rms::GetFirmwareJobStatusRequest { job_id })
                    .await
                    .map(Response::into_inner)
            };
            match self.resolve_job(&gateway_job_id)? {
                None => Ok(Response::new(rms::GetFirmwareJobStatusResponse {
                    status: return_code(false),
                    error_message: job_not_found(&gateway_job_id),
                    job_id: gateway_job_id,
                    ..rms::GetFirmwareJobStatusResponse::default()
                })),
                Some(JobRecord::Single(job)) => {
                    let backend = self.backend(&job.source)?;
                    let mut response = self
                        .bounded(poll(backend, job.job_id.clone()))
                        .await
                        .map_err(|status| backend_error(&job.source, status))?;
                    response.error_message = self.rewrite_not_found(&job, &response.error_message);
                    response.job_id = gateway_job_id;
                    Ok(Response::new(response))
                }
                Some(JobRecord::Aggregate(parts)) => {
                    let responses = self.poll_parts(&parts, poll).await?;
                    let phases: Vec<JobPhase> = responses
                        .iter()
                        .map(|response| JobPhase::from_firmware_job_state(response.job_state))
                        .collect();
                    let deciding = &responses[JobPhase::deciding_index(&phases).unwrap_or_default()];
                    Ok(Response::new(rms::GetFirmwareJobStatusResponse {
                        status: return_code(all_success(responses.iter().map(|r| r.status))),
                        job_id: gateway_job_id,
                        job_state: deciding.job_state,
                        state_description: deciding.state_description.clone(),
                        rack_id: String::new(),
                        node_id: String::new(),
                        error_code: deciding.error_code,
                        error_message: join_errors(parts.iter().zip(&responses).map(
                            |(part, response)| {
                                (&part.source, self.rewrite_not_found(part, &response.error_message))
                            },
                        )),
                        result_json: String::new(),
                        created_at: None,
                        updated_at: None,
                    }))
                }
            }
        }

        /// Node batch like [`RackManager::apply_firmware_object`], with the image name from the
        /// same first instance.
        async fn apply_switch_system_image(
            &self,
            request: Request<rms::ApplySwitchSystemImageRequest>,
        ) -> Result<Response<rms::ApplySwitchSystemImageResponse>, Status> {
            let request = request.into_inner();
            let nodes = requested_nodes(&request.nodes);
            let fan = self
                .fan_out(&nodes, |mut backend, subset| {
                    let mut part = request.clone();
                    part.nodes = Some(rms::NodeSet { nodes: subset });
                    async move {
                        backend
                            .v1
                            .apply_switch_system_image(part)
                            .await
                            .map(Response::into_inner)
                    }
                })
                .await?;

            let jobs = self.node_jobs(
                &nodes,
                &fan,
                |response| response.jobs.as_slice(),
                |job| (&job.node_id, &job.job_id),
            );
            let first = first_answer(&fan);
            Ok(Response::new(rms::ApplySwitchSystemImageResponse {
                response: Some(self.merge_batches(&nodes, &fan, |r| r.response.as_ref())),
                object_id: first.map(|r| r.object_id.clone()).unwrap_or_default(),
                image_filename: first.map(|r| r.image_filename.clone()).unwrap_or_default(),
                jobs: jobs
                    .into_iter()
                    .map(|(node_id, job_id)| rms::SwitchSystemImageUpdateJobInfo { node_id, job_id })
                    .collect(),
            }))
        }

        /// Resolves the gateway job id like [`RackManager::get_firmware_job_status`], with the
        /// state spelled as a string.
        async fn get_switch_system_image_job_status(
            &self,
            request: Request<rms::GetSwitchSystemImageJobStatusRequest>,
        ) -> Result<Response<rms::GetSwitchSystemImageJobStatusResponse>, Status> {
            let gateway_job_id = request.into_inner().job_id;
            let poll = |mut backend: Backend, job_id: String| async move {
                backend
                    .v1
                    .get_switch_system_image_job_status(
                        rms::GetSwitchSystemImageJobStatusRequest { job_id },
                    )
                    .await
                    .map(Response::into_inner)
            };
            match self.resolve_job(&gateway_job_id)? {
                None => Ok(Response::new(rms::GetSwitchSystemImageJobStatusResponse {
                    status: return_code(false),
                    error_message: job_not_found(&gateway_job_id),
                    job_id: gateway_job_id,
                    ..rms::GetSwitchSystemImageJobStatusResponse::default()
                })),
                Some(JobRecord::Single(job)) => {
                    let backend = self.backend(&job.source)?;
                    let mut response = self
                        .bounded(poll(backend, job.job_id.clone()))
                        .await
                        .map_err(|status| backend_error(&job.source, status))?;
                    response.error_message = self.rewrite_not_found(&job, &response.error_message);
                    response.job_id = gateway_job_id;
                    Ok(Response::new(response))
                }
                Some(JobRecord::Aggregate(parts)) => {
                    let responses = self.poll_parts(&parts, poll).await?;
                    let phases: Vec<JobPhase> = responses
                        .iter()
                        .map(|response| JobPhase::from_wire_str(&response.state))
                        .collect();
                    let deciding = &responses[JobPhase::deciding_index(&phases).unwrap_or_default()];
                    Ok(Response::new(rms::GetSwitchSystemImageJobStatusResponse {
                        status: return_code(all_success(responses.iter().map(|r| r.status))),
                        job_id: gateway_job_id,
                        state: deciding.state.clone(),
                        message: deciding.message.clone(),
                        rack_id: String::new(),
                        node_id: String::new(),
                        error_message: join_errors(parts.iter().zip(&responses).map(
                            |(part, response)| {
                                (&part.source, self.rewrite_not_found(part, &response.error_message))
                            },
                        )),
                        result_json: String::new(),
                        created_at: None,
                        updated_at: None,
                    }))
                }
            }
        }

        /// Resolves the gateway job id. A single-instance job is forwarded and its ids rewritten;
        /// an aggregate polls every per-instance parent and reports the worst, least advanced of
        /// them, listing the parents as its children; an unknown id is reported complete.
        async fn get_job_status(
            &self,
            request: Request<rms::GetJobStatusRequest>,
        ) -> Result<Response<rms::GetJobStatusResponse>, Status> {
            let request = request.into_inner();
            let include_children = request.include_child_job_states;
            match self.resolve_job(&request.job_id)? {
                None => Ok(Response::new(rms::GetJobStatusResponse {
                    job_states: vec![rms::JobStatus {
                        job_id: request.job_id,
                        execution_state: JobPhase::Completed.execution_state(),
                        state_description: JobPhase::Completed.wire_str().to_owned(),
                        ..rms::JobStatus::default()
                    }],
                })),
                Some(JobRecord::Single(job)) => {
                    let mut backend = self.backend(&job.source)?;
                    let response = self
                        .bounded(backend.v1.get_job_status(rms::GetJobStatusRequest {
                            job_id: job.job_id.clone(),
                            include_child_job_states: include_children,
                        }))
                        .await
                        .map_err(|status| backend_error(&job.source, status))?
                        .into_inner();
                    Ok(Response::new(rms::GetJobStatusResponse {
                        job_states: response
                            .job_states
                            .into_iter()
                            .map(|status| self.translate_job_status(&job.source, status))
                            .collect(),
                    }))
                }
                Some(JobRecord::Aggregate(parts)) => {
                    let responses = self
                        .poll_parts(&parts, |mut backend, job_id| async move {
                            backend
                                .v1
                                .get_job_status(rms::GetJobStatusRequest {
                                    job_id,
                                    include_child_job_states: include_children,
                                })
                                .await
                                .map(Response::into_inner)
                        })
                        .await?;

                    // Each part's own status is the entry for the id that was polled; an instance
                    // that echoes nothing usable counts as unknown, never as done.
                    let own: Vec<rms::JobStatus> = parts
                        .iter()
                        .zip(&responses)
                        .map(|(part, response)| {
                            response
                                .job_states
                                .iter()
                                .find(|status| status.job_id == part.job_id)
                                .or_else(|| response.job_states.first())
                                .cloned()
                                .unwrap_or_else(|| rms::JobStatus {
                                    job_id: part.job_id.clone(),
                                    ..rms::JobStatus::default()
                                })
                        })
                        .collect();
                    let phases: Vec<JobPhase> = own
                        .iter()
                        .map(|status| JobPhase::from_execution_state(status.execution_state))
                        .collect();
                    let deciding = &own[JobPhase::deciding_index(&phases).unwrap_or_default()];

                    let parent = rms::JobStatus {
                        job_id: request.job_id.clone(),
                        parent_job_id: None,
                        child_job_ids: parts
                            .iter()
                            .map(|part| self.jobs.intern(&part.source, &part.job_id))
                            .collect(),
                        execution_state: deciding.execution_state,
                        error_message: join_errors(
                            parts
                                .iter()
                                .zip(&own)
                                .map(|(part, status)| (&part.source, status.error_message.as_str())),
                        ),
                        error_code: deciding.error_code,
                        result_json: String::new(),
                        state_description: deciding.state_description.clone(),
                        rack_id: None,
                        node_id: None,
                        created_at: None,
                        updated_at: None,
                    };

                    let mut job_states = vec![parent];
                    if include_children {
                        for (part, response) in parts.iter().zip(responses) {
                            job_states.extend(
                                response
                                    .job_states
                                    .into_iter()
                                    .map(|status| self.translate_job_status(&part.source, status)),
                            );
                        }
                    }
                    Ok(Response::new(rms::GetJobStatusResponse { job_states }))
                }
            }
        }

        /// Resolves the gateway job id like [`RackManager::get_job_status`], with the state spelled
        /// as a string.
        async fn get_configure_switch_certificate_job_status(
            &self,
            request: Request<rms::GetConfigureSwitchCertificateJobStatusRequest>,
        ) -> Result<Response<rms::GetConfigureSwitchCertificateJobStatusResponse>, Status> {
            let gateway_job_id = request.into_inner().job_id;
            let poll = |mut backend: Backend, job_id: String| async move {
                backend
                    .v1
                    .get_configure_switch_certificate_job_status(
                        rms::GetConfigureSwitchCertificateJobStatusRequest { job_id },
                    )
                    .await
                    .map(Response::into_inner)
            };
            match self.resolve_job(&gateway_job_id)? {
                None => Ok(Response::new(rms::GetConfigureSwitchCertificateJobStatusResponse {
                    status: return_code(true),
                    job_id: gateway_job_id,
                    state: JobPhase::Completed.wire_str().to_owned(),
                    ..rms::GetConfigureSwitchCertificateJobStatusResponse::default()
                })),
                Some(JobRecord::Single(job)) => {
                    let backend = self.backend(&job.source)?;
                    let mut response = self
                        .bounded(poll(backend, job.job_id))
                        .await
                        .map_err(|status| backend_error(&job.source, status))?;
                    response.job_id = gateway_job_id;
                    Ok(Response::new(response))
                }
                Some(JobRecord::Aggregate(parts)) => {
                    let responses = self.poll_parts(&parts, poll).await?;
                    let phases: Vec<JobPhase> = responses
                        .iter()
                        .map(|response| JobPhase::from_wire_str(&response.state))
                        .collect();
                    let deciding = &responses[JobPhase::deciding_index(&phases).unwrap_or_default()];
                    Ok(Response::new(rms::GetConfigureSwitchCertificateJobStatusResponse {
                        status: return_code(all_success(responses.iter().map(|r| r.status))),
                        job_id: gateway_job_id,
                        state: deciding.state.clone(),
                        message: deciding.message.clone(),
                        rack_id: String::new(),
                        node_id: String::new(),
                        error_message: join_errors(
                            parts
                                .iter()
                                .zip(&responses)
                                .map(|(part, response)| (&part.source, response.error_message.as_str())),
                        ),
                        result_json: String::new(),
                        created_at: None,
                        updated_at: None,
                    }))
                }
            }
        }
    }

    unimplemented {
        set_power_state(SetPowerStateRequest) -> SetPowerStateResponse,
        get_power_state(GetPowerStateRequest) -> GetPowerStateResponse,
        sequence_rack_power(SequenceRackPowerRequest) -> SequenceRackPowerResponse,
        list_node_inventory(ListNodeInventoryRequest) -> ListNodeInventoryResponse,
        create_nodes(CreateNodesRequest) -> CreateNodesResponse,
        update_node(UpdateNodeRequest) -> UpdateNodeResponse,
        delete_node(DeleteNodeRequest) -> DeleteNodeResponse,
        get_rack_power_on_sequence(GetRackPowerOnSequenceRequest) -> GetRackPowerOnSequenceResponse,
        set_rack_power_on_sequence(SetRackPowerOnSequenceRequest) -> SetRackPowerOnSequenceResponse,
        list_racks(ListRacksRequest) -> ListRacksResponse,
        get_node_device_info(GetNodeDeviceInfoRequest) -> GetNodeDeviceInfoResponse,
        list_node_device_info_by_node_type(ListNodeDeviceInfoByNodeTypeRequest) -> ListNodeDeviceInfoByNodeTypeResponse,
        get_node_firmware_inventory(GetNodeFirmwareInventoryRequest) -> GetNodeFirmwareInventoryResponse,
        update_firmware(UpdateFirmwareRequest) -> UpdateFirmwareResponse,
        batch_update_firmware_by_node_type(BatchUpdateFirmwareByNodeTypeRequest) -> BatchUpdateFirmwareByNodeTypeResponse,
        batch_update_firmware(BatchUpdateFirmwareRequest) -> BatchUpdateFirmwareResponse,
        update_switch_system_image(UpdateSwitchSystemImageRequest) -> UpdateSwitchSystemImageResponse,
        get_rack_firmware_inventory(GetRackFirmwareInventoryRequest) -> GetRackFirmwareInventoryResponse,
        add_firmware_object(AddFirmwareObjectRequest) -> AddFirmwareObjectResponse,
        get_firmware_object(GetFirmwareObjectRequest) -> GetFirmwareObjectResponse,
        delete_firmware_object(DeleteFirmwareObjectRequest) -> DeleteFirmwareObjectResponse,
        set_default_firmware_object(SetDefaultFirmwareObjectRequest) -> SetDefaultFirmwareObjectResponse,
        apply_stored_firmware_object(ApplyStoredFirmwareObjectRequest) -> ApplyStoredFirmwareObjectResponse,
        apply_stored_switch_system_image(ApplyStoredSwitchSystemImageRequest) -> ApplyStoredSwitchSystemImageResponse,
        get_firmware_object_history(GetFirmwareObjectHistoryRequest) -> GetFirmwareObjectHistoryResponse,
        list_switch_firmware(ListSwitchFirmwareRequest) -> ListSwitchFirmwareResponse,
        push_switch_firmware(PushSwitchFirmwareRequest) -> PushSwitchFirmwareResponse,
        configure_scale_up_fabric_manager(ConfigureScaleUpFabricManagerRequest) -> ConfigureScaleUpFabricManagerResponse,
        batch_reset_switch_sdn_factory_default(BatchResetSwitchSdnFactoryDefaultRequest) -> BatchResetSwitchSdnFactoryDefaultResponse,
        get_scale_up_fabric_state(GetScaleUpFabricStateRequest) -> GetScaleUpFabricStateResponse,
        batch_set_scale_up_fabric_state(BatchSetScaleUpFabricStateRequest) -> BatchSetScaleUpFabricStateResponse,
        set_scale_up_fabric_telemetry_interface_state(SetScaleUpFabricTelemetryInterfaceStateRequest) -> SetScaleUpFabricTelemetryInterfaceStateResponse,
        batch_disable_switch_mtls(BatchDisableSwitchMtlsRequest) -> BatchDisableSwitchMtlsResponse,
        list_switch_system_images(ListSwitchSystemImagesRequest) -> ListSwitchSystemImagesResponse,
    }
}

#[tonic::async_trait]
impl RackManagerV2 for RmsProxy {
    /// Rack-scoped: forwarded to the rack's owner, with the job id it returns mapped to a gateway
    /// id that `GetJobStatus` resolves.
    async fn configure_scale_up_fabric_manager(
        &self,
        request: Request<rms_v2::ConfigureScaleUpFabricManagerRequest>,
    ) -> Result<Response<rms_v2::ConfigureScaleUpFabricManagerResponse>, Status> {
        let request = request.into_inner();
        let mut backend = self.rack_owner(&requested_nodes(&request.nodes))?;
        let response = self
            .bounded(backend.v2.configure_scale_up_fabric_manager(request))
            .await
            .map_err(|status| backend_error(&backend.source, status))?
            .into_inner();
        Ok(Response::new(
            rms_v2::ConfigureScaleUpFabricManagerResponse {
                job_id: self.jobs.intern(&backend.source, &response.job_id),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::{Case, Check, check_cases, check_values};

    use super::*;
    use crate::ownership::OwnershipHandle;
    use crate::sources::Source;

    fn node(node_id: &str) -> rms::NodeInfo {
        rms::NodeInfo {
            node_id: node_id.to_owned(),
            ..rms::NodeInfo::default()
        }
    }

    fn rack_node(node_id: &str, rack_id: &str) -> rms::NodeInfo {
        rms::NodeInfo {
            rack_id: rack_id.to_owned(),
            ..node(node_id)
        }
    }

    fn source(name: &str) -> SourceId {
        SourceId(name.to_owned())
    }

    fn result(node_id: &str, error_message: &str) -> rms::NodeOperationResult {
        rms::NodeOperationResult {
            node_id: node_id.to_owned(),
            status: rms::ReturnCode::Success as i32,
            error_message: error_message.to_owned(),
        }
    }

    fn messages<'a>(answers: &[Option<&'a rms::NodeOperationResult>]) -> Vec<Option<&'a str>> {
        answers
            .iter()
            .map(|answer| answer.map(|result| result.error_message.as_str()))
            .collect()
    }

    fn part<T>(name: &str, indices: Vec<usize>, outcome: Result<T, Status>) -> Part<T> {
        Part {
            source: source(name),
            indices,
            outcome,
        }
    }

    /// A proxy over `ownership` with the instances at `sources` bound; nothing is connected.
    fn proxy_over(
        ownership: impl Ownership + 'static,
        sources: &[(&str, String)],
        rms: &RmsConfig,
    ) -> RmsProxy {
        let proxy =
            RmsProxy::new(Arc::new(ownership), &SourceClientConfig::default(), rms).unwrap();
        let list = SourceList {
            generation: 1,
            ready: true,
            sources: sources
                .iter()
                .map(|(name, base_url)| Source {
                    name: (*name).to_owned(),
                    base_url: base_url.parse().unwrap(),
                    pod: String::new(),
                })
                .collect(),
        };
        proxy.bind_sources(&list).unwrap();
        proxy
    }

    /// A proxy over `ownership` with instances `names` bound at addresses nothing answers on.
    fn proxy(ownership: impl Ownership + 'static, names: &[&str]) -> RmsProxy {
        let sources: Vec<(&str, String)> = names
            .iter()
            .map(|name| (*name, format!("http://{name}.example:8443/")))
            .collect();
        proxy_over(ownership, &sources, &RmsConfig::default())
    }

    /// An ownership map fixed by the test; racks it does not name are unknown.
    struct Racks(HashMap<&'static str, Owner>);

    impl Ownership for Racks {
        fn owner_of_rack(&self, rack_id: &str) -> Owner {
            self.0
                .get(rack_id.trim())
                .cloned()
                .unwrap_or(Owner::Unknown)
        }

        fn is_ready(&self) -> bool {
            true
        }

        fn blockers(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn answers_are_matched_by_node_id_when_distinct_and_by_position_otherwise() {
        let nodes = [node("n1"), node("n2"), node("n3")];
        let answers = [result("n3", "third"), result("n1", "first")];
        let matched = correlate(&nodes, &[0, 2, 1], &answers, |r| &r.node_id);
        assert_eq!(
            messages(&matched),
            vec![Some("first"), Some("third"), None],
            "n2 was not answered and stays unmatched"
        );

        let nodes = [node("dup"), node("dup"), node("")];
        let answers = [result("dup", "a"), result("dup", "b")];
        let matched = correlate(&nodes, &[0, 1], &answers, |r| &r.node_id);
        assert_eq!(messages(&matched), vec![Some("a"), Some("b")]);
        let answers = [result("", "x")];
        let matched = correlate(&nodes, &[2], &answers, |r| &r.node_id);
        assert_eq!(messages(&matched), vec![Some("x")]);

        let answers = [result("dup", "only one")];
        let matched = correlate(&nodes, &[0, 1], &answers, |r| &r.node_id);
        assert_eq!(
            messages(&matched),
            vec![None, None],
            "a count mismatch without distinct ids matches nothing"
        );
    }

    #[test]
    fn merged_entries_follow_request_order_and_name_the_answering_instance() {
        let nodes = [node("n1"), node("n2"), node("n3"), node("n4")];
        let fan = FanOut {
            parts: vec![
                part("mat-b", vec![1], Ok(vec![result("n2", "from b")])),
                part("mat-a", vec![0, 2], Ok(vec![result("n3", "from a")])),
                part("mat-c", vec![3], Err(Status::unavailable("down"))),
            ],
            unowned: Vec::new(),
        };
        let merged: Vec<Option<(&str, &str)>> =
            merge_entries(&nodes, &fan, Vec::as_slice, |r| &r.node_id)
                .into_iter()
                .map(|entry| entry.map(|(source, r)| (source.0.as_str(), r.error_message.as_str())))
                .collect();
        assert_eq!(
            merged,
            vec![
                None,
                Some(("mat-b", "from b")),
                Some(("mat-a", "from a")),
                None
            ],
            "n1 was not answered and n4's instance failed"
        );
    }

    #[test]
    fn catalogues_are_merged_by_object_id_in_first_seen_order() {
        let object = |id: &str| rms::FirmwareObject {
            id: id.to_owned(),
            ..rms::FirmwareObject::default()
        };
        let merged = union_by_id(vec![
            vec![object("fw-1"), object("fw-2")],
            Vec::new(),
            vec![object("fw-2"), object("fw-3")],
        ]);
        assert_eq!(
            merged.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
            ["fw-1", "fw-2", "fw-3"]
        );
    }

    #[test]
    fn an_instance_failing_a_batch_fails_only_its_own_nodes() {
        let proxy = proxy(OwnershipHandle::new(), &[]);
        let nodes = [node("n0"), node("n1"), node("n2")];
        let fan = FanOut {
            parts: vec![
                part(
                    "mat-a",
                    vec![0, 2],
                    Ok(rms::NodeBatchResponse {
                        status: rms::ReturnCode::Success as i32,
                        node_results: vec![result("n0", ""), result("n2", "")],
                        ..rms::NodeBatchResponse::default()
                    }),
                ),
                part(
                    "mat-b",
                    vec![1],
                    Err(Status::unavailable("connection refused")),
                ),
            ],
            unowned: Vec::new(),
        };

        let batch = proxy.merge_batches(&nodes, &fan, |batch| Some(batch));

        let reason = "machine-a-tron mat-b: connection refused (Unavailable)";
        let results: Vec<(&str, i32, &str)> = batch
            .node_results
            .iter()
            .map(|r| (r.node_id.as_str(), r.status, r.error_message.as_str()))
            .collect();
        assert_eq!(
            results,
            vec![
                ("n0", rms::ReturnCode::Success as i32, ""),
                ("n1", rms::ReturnCode::Failure as i32, reason),
                ("n2", rms::ReturnCode::Success as i32, ""),
            ]
        );
        assert_eq!(batch.status, rms::ReturnCode::Failure as i32);
        assert_eq!(batch.message, reason);
        let stats = batch.stats.unwrap();
        assert_eq!(
            (
                stats.total_nodes,
                stats.successful_nodes,
                stats.failed_nodes
            ),
            (3, 2, 1)
        );
        assert_eq!(batch.job_id, "", "no instance issued a job");
    }

    #[test]
    fn a_request_every_instance_refuses_is_refused_and_a_mixed_fan_out_is_not() {
        let refusal = |name: &str| {
            part::<()>(
                name,
                vec![0],
                Err(Status::invalid_argument("power operation is unspecified")),
            )
        };
        check_values(
            [
                Check {
                    scenario: "every instance refuses",
                    input: vec![refusal("mat-a"), refusal("mat-b")],
                    expect: Some((
                        Code::InvalidArgument,
                        "machine-a-tron mat-a: power operation is unspecified".to_owned(),
                    )),
                },
                Check {
                    scenario: "one instance refuses and another fails otherwise",
                    input: vec![
                        refusal("mat-a"),
                        part("mat-b", vec![1], Err(Status::unavailable("down"))),
                    ],
                    expect: None,
                },
                Check {
                    scenario: "no instance was asked",
                    input: Vec::new(),
                    expect: None,
                },
            ],
            |parts| {
                refused_by_every_instance(&FanOut {
                    parts,
                    unowned: Vec::new(),
                })
                .map(|status| (status.code(), status.message().to_owned()))
            },
        );
    }

    #[test]
    fn an_instance_forgetting_a_job_names_it_by_its_gateway_id() {
        let proxy = proxy(OwnershipHandle::new(), &[]);
        let job = BackendJob {
            source: source("mat-a"),
            job_id: "rms-mock-7-3".to_owned(),
        };
        let gateway_id = proxy.jobs.intern(&job.source, &job.job_id);
        check_values(
            [
                Check {
                    scenario: "the instance has forgotten the job",
                    input: "job rms-mock-7-3 not found",
                    expect: format!("job {gateway_id} not found"),
                },
                Check {
                    scenario: "another job's message is left alone",
                    input: "job rms-mock-7-30 not found",
                    expect: "job rms-mock-7-30 not found".to_owned(),
                },
            ],
            |message| proxy.rewrite_not_found(&job, message),
        );
    }

    #[tokio::test]
    async fn a_call_the_instance_never_answers_fails_within_the_request_timeout() {
        // Accepts connections and never reads them.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let request_timeout = Duration::from_millis(200);
        let proxy = proxy_over(
            Racks(HashMap::from([(
                "rack-001",
                Owner::Source(source("mat-a")),
            )])),
            &[(
                "mat-a",
                format!("http://{}/", listener.local_addr().unwrap()),
            )],
            &RmsConfig { request_timeout },
        );
        let request = rms::BatchGetPowerStateRequest {
            nodes: Some(rms::NodeSet {
                nodes: vec![rack_node("s1", "rack-001")],
            }),
        };

        let response = tokio::time::timeout(
            request_timeout * 10,
            proxy.batch_get_power_state(Request::new(request)),
        )
        .await
        .expect("the request timeout bounds the call")
        .unwrap()
        .into_inner();

        let batch = response.response.unwrap();
        assert_eq!(
            batch
                .node_results
                .iter()
                .map(|result| result.error_message.as_str())
                .collect::<Vec<_>>(),
            ["machine-a-tron mat-a: no answer within 200ms (DeadlineExceeded)"],
            "{batch:?}"
        );
    }

    #[tokio::test]
    async fn a_rack_scoped_request_goes_to_the_one_instance_owning_its_racks() {
        let proxy = proxy(
            Racks(HashMap::from([
                ("rack-001", Owner::Source(source("mat-a"))),
                ("rack-003", Owner::Source(source("mat-a"))),
                ("rack-002", Owner::Source(source("mat-b"))),
                ("rack-004", Owner::Dropped(source("mat-b"))),
                (
                    "rack-shared",
                    Owner::Ambiguous(vec![source("mat-a"), source("mat-b")]),
                ),
            ])),
            &["mat-a", "mat-b"],
        );
        let refused = |code: Code, message: &str| FailsWith((code, message.to_owned()));

        check_cases(
            [
                Case {
                    scenario: "two racks of one instance are forwarded to it",
                    input: vec![rack_node("s1", "rack-001"), rack_node("s3", "rack-003")],
                    expect: Yields(source("mat-a")),
                },
                Case {
                    scenario: "racks of two instances",
                    input: vec![rack_node("s1", "rack-001"), rack_node("s2", "rack-002")],
                    expect: refused(
                        Code::InvalidArgument,
                        "the nodes span rack \"rack-001\" on machine-a-tron mat-a and rack \"rack-002\"; a rack-scoped request must name racks of one instance",
                    ),
                },
                Case {
                    scenario: "a routable first rack and a dropped later rack keep UNAVAILABLE",
                    input: vec![rack_node("s1", "rack-001"), rack_node("s4", "rack-004")],
                    expect: refused(
                        Code::Unavailable,
                        "machine-a-tron mat-b owns rack \"rack-004\" but has not answered its status polls for ownership.stale_after; retry once it answers",
                    ),
                },
                Case {
                    scenario: "a routable first rack and an unknown later rack keep NOT_FOUND",
                    input: vec![rack_node("s1", "rack-001"), rack_node("s9", "rack-999")],
                    expect: refused(
                        Code::NotFound,
                        "no machine-a-tron instance owns rack \"rack-999\"",
                    ),
                },
                Case {
                    scenario: "a rack whose owner was dropped",
                    input: vec![rack_node("s4", "rack-004")],
                    expect: refused(
                        Code::Unavailable,
                        "machine-a-tron mat-b owns rack \"rack-004\" but has not answered its status polls for ownership.stale_after; retry once it answers",
                    ),
                },
                Case {
                    scenario: "a rack two instances report",
                    input: vec![rack_node("s5", "rack-shared")],
                    expect: refused(
                        Code::InvalidArgument,
                        "rack \"rack-shared\" is reported by machine-a-tron instances mat-a and mat-b",
                    ),
                },
                Case {
                    scenario: "a rack nobody reports",
                    input: vec![rack_node("s9", "rack-999")],
                    expect: refused(
                        Code::NotFound,
                        "no machine-a-tron instance owns rack \"rack-999\"",
                    ),
                },
                Case {
                    scenario: "a blank rack id",
                    input: vec![rack_node("s1", " ")],
                    expect: refused(Code::InvalidArgument, "node \"s1\" names no rack"),
                },
                Case {
                    scenario: "no nodes",
                    input: Vec::new(),
                    expect: refused(Code::InvalidArgument, "the request names no nodes"),
                },
            ],
            |nodes| {
                proxy
                    .rack_owner(&nodes)
                    .map(|backend| backend.source)
                    .map_err(|status| (status.code(), status.message().to_owned()))
            },
        );
    }
}
