// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

mod observation;

use std::collections::BTreeMap;

use carbide_uuid::machine::DpuMachineId;
use futures::{StreamExt as _, stream};
use k8s_openapi::api::core::v1::Node;
use tokio::time::Instant;

use super::{
    DPF_MANAGEMENT_REQUEST_TIMEOUT, DpfSdk, bounded_management_request, dpu_device_cr_name,
};
use crate::DpfError;
use crate::crds::dpus_generated::DPU;
use crate::repository::{DpuClusterRepository, DpuRepository, ServiceVpcClusterRepository};
use crate::types::{DPU_MACHINE_ID_LABEL, DpuClusterIdentity, ServiceVpcNodeIdentity};

// Keep at most four DPU clusters in flight while sharing reads across every requested DPU in each.
const OBSERVER_CLUSTER_CONCURRENCY: usize = 4;

/// Retains the management DPU UID and explicit cluster mapping for the final lookup revalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DpuNodeMapping {
    dpu_name: String,
    dpu_uid: String,
    cluster_namespace: String,
    cluster_name: String,
}

impl<R: DpuRepository + DpuClusterRepository + ServiceVpcClusterRepository, L> DpfSdk<R, L> {
    /// Resolves remote Node UIDs through the current management DPU and DPUCluster mappings.
    ///
    /// Callers must pair each persisted DPU ID with its raw device ID (without device-), ensure
    /// declared Node identity propagation, and retain the result before allowing ServiceChain
    /// creation on those Nodes.
    /// A retained baseline must not be replaced during retirement. Empty input performs no I/O.
    ///
    /// Reads are batched by actual DPUCluster, with at most four clusters in flight. Every
    /// management read, credential lookup and complete Node list retains its ten-second attempt limit.
    /// The caller's `deadline` bounds the method. Initial management and cluster reads reserve the
    /// smaller of ten seconds or one third of the initial remaining time for the final management
    /// revalidation. After the initial list, each cluster receives at most one equal share of the
    /// remaining cluster time across the required four-cluster waves, measured from its actual start.
    /// Missing, ambiguous or deleting identities and management mappings that change during the
    /// remote reads omit only the affected DPUs. A failed cluster read omits that cluster's DPUs;
    /// a failed shared management list fails the lookup. Callers must treat omitted DPUs as pending.
    /// Subsequent chain observations must verify the retained Node UID again. This operation does
    /// not mutate Kubernetes or persisted state.
    pub async fn resolve_service_vpc_nodes(
        &self,
        devices: &BTreeMap<DpuMachineId, String>,
        deadline: Instant,
    ) -> Result<BTreeMap<DpuMachineId, ServiceVpcNodeIdentity>, DpfError> {
        if devices.is_empty() {
            return Ok(BTreeMap::new());
        }

        // Resolve every device from one complete management list before contacting DPU clusters.
        let remaining = deadline.saturating_duration_since(Instant::now());
        let cluster_cutoff = deadline - DPF_MANAGEMENT_REQUEST_TIMEOUT.min(remaining / 3);
        let initial_dpus = service_vpc_request_until(
            cluster_cutoff,
            "list initial service-VPC DPU mappings",
            read_dpu_mappings(&*self.repo, &self.namespace),
        )
        .await?;
        let mappings = devices
            .iter()
            .filter_map(|(dpu_id, device)| {
                match dpu_node_mapping(&initial_dpus, device, *dpu_id) {
                    Ok(mapping) => Some((*dpu_id, mapping)),
                    Err(error) => {
                        tracing::warn!(dpu_machine_id = %dpu_id, %error, "service VPC management mapping is unavailable");
                        None
                    }
                }
            })
            .collect::<BTreeMap<_, _>>();
        let mut clusters = BTreeMap::<_, Vec<_>>::new();
        for (dpu_id, mapping) in &mappings {
            clusters
                .entry((
                    mapping.cluster_namespace.clone(),
                    mapping.cluster_name.clone(),
                ))
                .or_default()
                .push((*dpu_id, mapping));
        }

        // Use the DPF-issued observer for each actual cluster, never the management client.
        // Build the futures first so borrowed DPU mappings do not constrain the buffered stream's Send implementation.
        let waves = u32::try_from(clusters.len().div_ceil(OBSERVER_CLUSTER_CONCURRENCY))
            .unwrap_or(u32::MAX)
            .max(1);
        let cluster_budget = cluster_cutoff.saturating_duration_since(Instant::now()) / waves;
        let cluster_reads = clusters
            .into_iter()
            .map(|((namespace, name), targets)| async move {
                // Leave a share for queued clusters when every earlier observer stalls.
                let cluster_deadline = cluster_cutoff.min(Instant::now() + cluster_budget);
                let lookup = async {
                    let cluster = read_cluster_identity(&*self.repo, &namespace, &name).await?;
                    let reader = bounded_management_request(
                        "open service-VPC Node reader",
                        self.repo.service_vpc_cluster_reader(&cluster),
                    )
                    .await?;
                    let nodes = reader.list_nodes().await?;

                    let nodes_by_dpu = index_nodes_by_dpu(nodes.items);
                    // Reject same-name cluster replacement while the remote Node list was in flight.
                    if read_cluster_identity(&*self.repo, &namespace, &name).await? != cluster {
                        return Err(DpfError::InvalidState(
                            "DPUCluster changed during Node lookup".to_owned(),
                        ));
                    }
                    Ok(targets
                        .into_iter()
                        .filter_map(|(dpu_id, mapping)| {
                            match observed_node_identity(&cluster, &nodes_by_dpu, dpu_id, &mapping.dpu_name) {
                                Ok(node) => Some((dpu_id, node)),
                                Err(error) => {
                                    tracing::warn!(dpu_machine_id = %dpu_id, %error, "service VPC Node identity is unavailable");
                                    None
                                }
                            }
                        })
                        .collect::<Vec<_>>())
                };
                let result = service_vpc_request_until(
                    cluster_deadline,
                    "resolve service-VPC cluster Nodes",
                    lookup,
                )
                .await;
                match result {
                    Ok(nodes) => nodes,
                    Err(error) => {
                        tracing::warn!(cluster_namespace = %namespace, cluster_name = %name, %error, "service VPC cluster Node lookup failed");
                        Vec::new()
                    }
                }
            })
            .collect::<Vec<_>>();
        let resolved = stream::iter(cluster_reads)
            .buffer_unordered(OBSERVER_CLUSTER_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        // No identity can authorize placement when every requested DPU remains unresolved.
        if resolved.iter().all(|nodes| nodes.is_empty()) {
            return Ok(BTreeMap::new());
        }

        // Reject DPU replacement or cluster reassignment that occurred during the remote reads.
        let current_dpus = service_vpc_request_until(
            deadline,
            "revalidate service-VPC DPU mappings",
            read_dpu_mappings(&*self.repo, &self.namespace),
        )
        .await?;
        Ok(resolved
            .into_iter()
            .flatten()
            .filter(|(dpu_id, _)| {
                let current = devices.get(dpu_id).and_then(|device| dpu_node_mapping(&current_dpus, device, *dpu_id).ok());
                let unchanged = current.as_ref() == mappings.get(dpu_id);
                if !unchanged {
                    tracing::warn!(dpu_machine_id = %dpu_id, "DPU mapping changed during service VPC Node lookup");
                }
                unchanged
            })
            .collect())
    }
}

/// Cancels Service VPC reads at the supplied deadline so callers can reserve final revalidation time.
async fn service_vpc_request_until<T>(
    deadline: Instant,
    operation: &'static str,
    request: impl std::future::Future<Output = Result<T, DpfError>>,
) -> Result<T, DpfError> {
    // Do not poll queued cluster reads once their allotted time has already elapsed.
    if Instant::now() >= deadline {
        return Err(DpfError::timeout(
            operation,
            "service-VPC read deadline exceeded",
        ));
    }
    tokio::time::timeout_at(deadline, request)
        .await
        .map_err(|_| DpfError::timeout(operation, "service-VPC read deadline exceeded"))?
}

/// Reads one live DPUCluster UID so same-name replacement cannot reuse an earlier routing identity.
async fn read_cluster_identity<R: DpuClusterRepository>(
    repo: &R,
    namespace: &str,
    name: &str,
) -> Result<DpuClusterIdentity, DpfError> {
    let cluster = bounded_management_request(
        "read service-VPC DPUCluster",
        DpuClusterRepository::get(repo, name, namespace),
    )
    .await?
    .ok_or_else(|| DpfError::not_found("DPUCluster", name))?;
    let uid = cluster
        .metadata
        .uid
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| DpfError::InvalidState("DPUCluster has no UID".to_owned()))?;
    if cluster.metadata.deletion_timestamp.is_some() {
        return Err(DpfError::InvalidState("DPUCluster is deleting".to_owned()));
    }
    Ok(DpuClusterIdentity {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        uid,
    })
}

/// Indexes one bounded management DPU list by device while retaining ambiguous incarnations.
async fn read_dpu_mappings<R: DpuRepository>(
    repo: &R,
    namespace: &str,
) -> Result<BTreeMap<String, Vec<DPU>>, DpfError> {
    let dpus = bounded_management_request(
        "list service-VPC DPU mappings",
        DpuRepository::list(repo, namespace, None),
    )
    .await?;
    let mut by_device = BTreeMap::<_, Vec<_>>::new();
    for dpu in dpus {
        by_device
            .entry(dpu.spec.dpu_device_name.clone())
            .or_default()
            .push(dpu);
    }
    Ok(by_device)
}

/// Selects one current DPU mapping without guessing a cluster or accepting conflicting DPU identity.
fn dpu_node_mapping(
    dpus: &BTreeMap<String, Vec<DPU>>,
    raw_device_id: &str,
    dpu_id: DpuMachineId,
) -> Result<DpuNodeMapping, DpfError> {
    let candidates = dpus
        .get(&dpu_device_cr_name(raw_device_id))
        .map(Vec::as_slice)
        .unwrap_or_default();
    let [dpu] = candidates else {
        return Err(DpfError::InvalidState(format!(
            "DPU {dpu_id} has a missing or ambiguous management mapping"
        )));
    };
    let identity = dpu_id.to_string();
    // Check a present management label; remote Node validation still requires the propagated identity.
    if dpu.metadata.deletion_timestamp.is_some()
        || dpu
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(DPU_MACHINE_ID_LABEL))
            .is_some_and(|label| label != &identity)
    {
        return Err(DpfError::InvalidState(format!(
            "DPU {dpu_id} mapping is deleting or conflicts with its identity"
        )));
    }

    // DPF fills both fields after selecting a cluster; absence must not select a default cluster.
    let cluster =
        dpu.spec.cluster.as_ref().ok_or_else(|| {
            DpfError::InvalidState(format!("DPU {dpu_id} has no cluster mapping"))
        })?;
    let required = |value: &Option<String>| {
        value
            .as_ref()
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or_else(|| {
                DpfError::InvalidState(format!("DPU {dpu_id} has incomplete mapping identity"))
            })
    };
    Ok(DpuNodeMapping {
        dpu_name: required(&dpu.metadata.name)?,
        dpu_uid: required(&dpu.metadata.uid)?,
        cluster_namespace: required(&cluster.namespace)?,
        cluster_name: required(&cluster.name)?,
    })
}

/// Indexes remote Nodes by NICo identity while retaining duplicates that must block adoption.
fn index_nodes_by_dpu(nodes: Vec<Node>) -> BTreeMap<String, Vec<Node>> {
    let mut by_dpu = BTreeMap::<_, Vec<_>>::new();
    for node in nodes {
        if let Some(identity) = node
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(DPU_MACHINE_ID_LABEL))
            .cloned()
        {
            by_dpu.entry(identity).or_default().push(node);
        }
    }
    by_dpu
}

/// Checks one live Node against the DPU name and machine ID before returning its observed UID.
///
/// Callers must supply the complete Node inventory indexed by `index_nodes_by_dpu` and the
/// DPUCluster identity verified for these Node observations.
fn observed_node_identity(
    cluster: &DpuClusterIdentity,
    nodes: &BTreeMap<String, Vec<Node>>,
    dpu_id: DpuMachineId,
    dpu_name: &str,
) -> Result<ServiceVpcNodeIdentity, DpfError> {
    let candidates = nodes
        .get(&dpu_id.to_string())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let [node] = candidates else {
        return Err(DpfError::InvalidState(format!(
            "DPU {dpu_id} does not identify exactly one remote Node"
        )));
    };
    // DPF's GenerateNodeName uses DPU.metadata.name; the label alone is insufficient.
    if node.metadata.name.as_deref() != Some(dpu_name) || node.metadata.deletion_timestamp.is_some()
    {
        return Err(DpfError::InvalidState(format!(
            "DPU {dpu_id} remote Node mapping is stale or deleting"
        )));
    }
    let uid = node
        .metadata
        .uid
        .as_ref()
        .filter(|uid| !uid.is_empty())
        .ok_or_else(|| DpfError::InvalidState("remote Node has no UID".to_owned()))?;
    Ok(ServiceVpcNodeIdentity {
        cluster: cluster.clone(),
        name: dpu_name.to_owned(),
        uid: uid.clone(),
    })
}
