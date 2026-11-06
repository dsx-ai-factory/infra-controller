// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

mod observation;

use std::collections::BTreeMap;

use carbide_uuid::machine::DpuMachineId;
use futures::{StreamExt as _, TryStreamExt as _, stream};
use k8s_openapi::api::core::v1::Node;

use super::{DpfSdk, bounded_management_request, dpu_device_cr_name};
use crate::DpfError;
use crate::crds::dpus_generated::DPU;
use crate::repository::{DpuClusterRepository, DpuRepository, ServiceVpcClusterRepository};
use crate::types::{DPU_MACHINE_ID_LABEL, DpuClusterIdentity, ServiceVpcNodeIdentity};

// Keep at most four DPU clusters in flight while sharing reads across every requested DPU in each.
const OBSERVER_CLUSTER_CONCURRENCY: usize = 4;

/// Retains the management DPU UID and explicit cluster mapping for the final lookup revalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DpuNodeRoute {
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
    /// management read, credential lookup and complete Node list has a ten-second deadline.
    /// Missing, ambiguous or deleting identities and management mappings that change during the
    /// remote reads fail the complete lookup without returning a partial map. Subsequent chain
    /// observations must verify the retained Node UID again. This operation does not mutate
    /// Kubernetes or persisted state.
    pub async fn resolve_service_vpc_nodes(
        &self,
        devices: &BTreeMap<DpuMachineId, String>,
    ) -> Result<BTreeMap<DpuMachineId, ServiceVpcNodeIdentity>, DpfError> {
        if devices.is_empty() {
            return Ok(BTreeMap::new());
        }

        // Resolve every device from one complete management list before contacting DPU clusters.
        let initial_dpus = read_dpu_routes(&*self.repo, &self.namespace).await?;
        let routes = devices
            .iter()
            .map(|(dpu_id, device)| {
                dpu_node_route(&initial_dpus, device, *dpu_id).map(|route| (*dpu_id, route))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let mut clusters = BTreeMap::<_, Vec<_>>::new();
        for (dpu_id, route) in &routes {
            clusters
                .entry((route.cluster_namespace.clone(), route.cluster_name.clone()))
                .or_default()
                .push((*dpu_id, route));
        }

        // Use the DPF-issued observer for each actual cluster, never the management client.
        // Build the futures first so borrowed DPU routes do not constrain the buffered stream's Send implementation.
        let cluster_reads = clusters
            .into_iter()
            .map(|((namespace, name), targets)| async move {
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
                targets
                    .into_iter()
                    .map(|(dpu_id, route)| {
                        observed_node_identity(&cluster, &nodes_by_dpu, dpu_id, &route.dpu_name)
                            .map(|node| (dpu_id, node))
                    })
                    .collect::<Result<Vec<_>, DpfError>>()
            })
            .collect::<Vec<_>>();
        let resolved = stream::iter(cluster_reads)
            .buffer_unordered(OBSERVER_CLUSTER_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;

        // Reject DPU replacement or cluster reassignment that occurred during the remote reads.
        let current_dpus = read_dpu_routes(&*self.repo, &self.namespace).await?;
        for (dpu_id, device) in devices {
            let current = dpu_node_route(&current_dpus, device, *dpu_id)?;
            if routes.get(dpu_id) != Some(&current) {
                return Err(DpfError::InvalidState(format!(
                    "DPU {dpu_id} mapping changed during service-VPC Node lookup"
                )));
            }
        }
        Ok(resolved.into_iter().flatten().collect())
    }
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
async fn read_dpu_routes<R: DpuRepository>(
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
fn dpu_node_route(
    dpus: &BTreeMap<String, Vec<DPU>>,
    raw_device_id: &str,
    dpu_id: DpuMachineId,
) -> Result<DpuNodeRoute, DpfError> {
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
    Ok(DpuNodeRoute {
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
/// Callers must supply the DPUCluster identity verified for these Node observations.
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
