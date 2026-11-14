// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use futures::{StreamExt as _, stream};
use k8s_openapi::api::core::v1::Node;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, ObjectMeta};
use kube::Resource;
use model::extension_service::{
    DPF_HELM_CHART_INTERFACE_ORDINAL_LABEL, DPF_HELM_CHART_OWNER_LABEL,
    DPF_HELM_CHART_PLACEMENT_LABEL_VALUE, DPF_HELM_CHART_SERVICE_VPC_SLOT_LABEL,
    DpfHelmChartIdentity,
};
use serde_json::Value;

use super::{
    OBSERVER_CLUSTER_CONCURRENCY, index_nodes_by_dpu, observed_node_identity, read_cluster_identity,
};
use crate::DpfError;
use crate::crds::dpuservicechains_generated::DPUServiceChain;
use crate::crds::servicechains_generated::ServiceChain;
use crate::crds::servicechainsets_generated::ServiceChainSet;
use crate::repository::{
    DpuClusterRepository, DpuServiceChainRepository, ServiceVpcClusterRepository,
};
use crate::sdk::{DpfSdk, bounded_management_request, registered_dpu_service_chain_spec};
use crate::types::{
    DpuClusterIdentity, SERVICE_VPC_CHAIN_REGISTRATION_UID_LABEL, ServiceVpcChainIntent,
    ServiceVpcChainObservation, ServiceVpcChainRequest, ServiceVpcChainState,
    ServiceVpcResourceObservation,
};

const SET_FINALIZER: &str = "dpu.nvidia.com/servicechainset";
const CHAIN_FINALIZER: &str = "svc.dpu.nvidia.com/ServiceChain-finalizer";
const SET_NAME_LABEL: &str = "svc.dpu.nvidia.com/servicechainset-name";
const SET_NAMESPACE_LABEL: &str = "svc.dpu.nvidia.com/servicechainset-namespace";

/// Retains each request and initial registration spec while remote reads are grouped by cluster.
struct ChainRead<'a> {
    index: usize,
    request: &'a ServiceVpcChainRequest,
    expected_spec: Value,
    observed: ServiceVpcChainObservation,
}

impl ServiceVpcChainObservation {
    /// Copies caller correlation before I/O so failures cannot be detached from their requested revision.
    fn pending(request: &ServiceVpcChainRequest) -> Self {
        Self {
            correlation: request.correlation.clone(),
            node: None,
            cleanup_bypass_observed: request.cleanup_bypass_observed,
            registration: None,
            set: None,
            chains: Vec::new(),
            chain_list_resource_version: None,
            state: ServiceVpcChainState::PendingCreation,
            reason: None,
        }
    }

    /// Updates the phase and diagnostic together so an invalidated proof cannot keep a success state.
    fn set_state(&mut self, state: ServiceVpcChainState, reason: Option<&'static str>) {
        self.reason = if matches!(state, ServiceVpcChainState::ReadFailure(_)) {
            None
        } else {
            reason
        };
        self.state = state;
    }

    /// Retains the original read failure alongside identities obtained before that read failed.
    fn read_failed(&mut self, error: Arc<DpfError>) {
        self.set_state(ServiceVpcChainState::ReadFailure(error), None);
    }
}

impl<R: DpuServiceChainRepository + DpuClusterRepository + ServiceVpcClusterRepository, L>
    DpfSdk<R, L>
{
    /// Observes current chain configuration or ordered exclusion evidence without changing lifecycle state.
    ///
    /// Callers must commit selector desire before this read and supply the corresponding definition,
    /// selector version, attachment/configuration correlation and Node identity retained before
    /// ServiceChain creation was enabled. Before consuming a result, callers must serialize
    /// revalidation of that persisted desire, correlation and retained Node identity with concurrent
    /// changes. Callers must also retain and resubmit cleanup_bypass_observed until administrative
    /// recovery, merging that fact monotonically when older completions arrive. This SDK does not
    /// read the database or authorize reclamation.
    ///
    /// Retirement requires the exact remote set spec and a current ServiceChainsReconciled=True
    /// condition before issuing a fresh, complete child list. Deleting or mismatched children block
    /// isolation. Conclusive results revalidate the Node, set, cluster and management registration.
    /// Activation checks the child's own ServiceChainReconciled and Ready generations; unrelated
    /// failures reconciling other Nodes in the same set do not gate this child's convergence.
    ///
    /// This is configuration evidence, not Pod, CNI, DHCP or traffic health. Supported withdrawal
    /// assumes the same existing Node UID, normal SFC finalizer cleanup and no other forwarding path.
    /// API absence cannot retrospectively detect every out-of-band finalizer bypass. Runtime
    /// dataplane qualification remains separate.
    ///
    /// Results preserve input order, one result per request, and unchanged caller correlation.
    /// Empty input performs no I/O. Management and remote reads have ten-second attempt deadlines,
    /// including complete remote pagination. At most four clusters are observed concurrently; each
    /// uses at most three concurrent reads. Failed reads remain ReadFailure evidence, never empty success.
    pub async fn observe_service_vpc_chains(
        &self,
        requests: &[ServiceVpcChainRequest],
    ) -> Vec<ServiceVpcChainObservation> {
        if requests.is_empty() {
            return Vec::new();
        }

        // Read registration desire once before dispatching batched remote observations.
        let registrations = match read_registrations(&*self.repo, &self.namespace).await {
            Ok(registrations) => registrations,
            Err(error) => {
                let error = Arc::new(error);
                return requests
                    .iter()
                    .map(|request| {
                        let mut observed = ServiceVpcChainObservation::pending(request);
                        observed.read_failed(error.clone());
                        observed
                    })
                    .collect();
            }
        };
        let mut results = Vec::with_capacity(requests.len());
        let mut groups = BTreeMap::<_, Vec<_>>::new();
        for (index, request) in requests.iter().enumerate() {
            let mut observed = ServiceVpcChainObservation::pending(request);
            observed.registration = registrations
                .get(&request.definition.name)
                .and_then(|registration| resource_observation(&registration.metadata));
            match prepare_registration(
                request,
                registrations.get(&request.definition.name),
                &self.namespace,
            ) {
                Ok((expected_spec, registration)) => {
                    observed.registration = Some(registration);
                    groups
                        .entry(request.node.cluster.clone())
                        .or_default()
                        .push(ChainRead {
                            index,
                            request,
                            expected_spec,
                            observed,
                        });
                }
                Err((state, reason)) => {
                    observed.set_state(state, Some(reason));
                    results.push((index, observed));
                }
            }
        }
        // Build the futures first so DpfOperations can send the buffered reads with their borrowed requests.
        let cluster_reads = groups
            .into_iter()
            .map(|(cluster, reads)| async move { self.observe_cluster(&cluster, reads).await })
            .collect::<Vec<_>>();
        let remote = stream::iter(cluster_reads)
            .buffer_unordered(OBSERVER_CLUSTER_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        results.extend(remote.into_iter().flatten());
        results.sort_by_key(|(index, _)| *index);
        let mut observations = results
            .into_iter()
            .map(|(_, observed)| observed)
            .collect::<Vec<_>>();

        // Revalidate the management UID, generation and exact desired spec after all remote proof reads.
        if observations.iter().any(|observed| {
            matches!(
                observed.state,
                ServiceVpcChainState::Converged | ServiceVpcChainState::Isolated
            )
        }) {
            match read_registrations(&*self.repo, &self.namespace).await {
                Ok(current) => {
                    for (request, observed) in requests.iter().zip(&mut observations) {
                        if !matches!(
                            observed.state,
                            ServiceVpcChainState::Converged | ServiceVpcChainState::Isolated
                        ) {
                            continue;
                        }
                        let still_current = match prepare_registration(
                            request,
                            current.get(&request.definition.name),
                            &self.namespace,
                        ) {
                            Ok((_, registration)) => {
                                observed.registration.as_ref().is_some_and(|initial| {
                                    initial.uid == registration.uid
                                        && initial.generation == registration.generation
                                })
                            }
                            Err((ServiceVpcChainState::ReadFailure(error), _)) => {
                                observed.read_failed(error);
                                continue;
                            }
                            Err(_) => false,
                        };
                        if !still_current {
                            observed.set_state(
                                ServiceVpcChainState::StaleIdentity,
                                Some(
                                    "management registration changed after the remote observations",
                                ),
                            );
                        }
                    }
                }
                Err(error) => {
                    let error = Arc::new(error);
                    for observed in &mut observations {
                        if matches!(
                            observed.state,
                            ServiceVpcChainState::Converged | ServiceVpcChainState::Isolated
                        ) {
                            observed.read_failed(error.clone());
                        }
                    }
                }
            }
        }
        observations
    }

    /// Orders remote reads for one cluster so retirement absence is always observed after acknowledgement.
    async fn observe_cluster(
        &self,
        cluster: &DpuClusterIdentity,
        mut reads: Vec<ChainRead<'_>>,
    ) -> Vec<(usize, ServiceVpcChainObservation)> {
        let result = async {
            // Verify retained cluster identity before opening its dedicated observer connection.
            if read_cluster_identity(&*self.repo, &cluster.namespace, &cluster.name).await? != *cluster {
                for read in &mut reads {
                    read.observed.set_state(ServiceVpcChainState::StaleIdentity, Some("retained DPUCluster UID changed"));
                }
                return Ok::<_, DpfError>(());
            }
            let reader = bounded_management_request(
                "open service-VPC chain reader", self.repo.service_vpc_cluster_reader(cluster)
            ).await?;
            let (nodes, sets) = tokio::try_join!(
                reader.list_nodes(), reader.list_service_chain_sets(&self.namespace)
            )?;
            let nodes = index_nodes_by_dpu(nodes.items);
            let sets = index_by_name(sets.items, "ServiceChainSet")?;
            let mut eligible = BTreeSet::new();
            for read in &mut reads {
                match validate_remote_snapshot(read.request, &read.expected_spec, &nodes, &sets, &mut read.observed) {
                    Ok(()) => { eligible.insert(read.index); }
                    Err((state, reason)) => read.observed.set_state(state, Some(reason)),
                }
            }
            if eligible.is_empty() { return Ok(()); }

            // Issue this authoritative list only after the required set acknowledgement was read.
            // Unfiltered children preserve deleting, old-registration and wrong-owner leftovers.
            let children = reader.list_service_chains(&self.namespace).await?;
            let mut by_node = BTreeMap::<_, Vec<_>>::new();
            for child in &children.items {
                by_node.entry(child.spec.node.as_deref().filter(|name| !name.is_empty()))
                    .or_default().push(child);
            }
            for read in &mut reads {
                if !eligible.contains(&read.index) { continue; }
                read.observed.chain_list_resource_version = children.metadata.resource_version.clone();
                let candidates = by_node.get(&Some(read.request.node.name.as_str())).into_iter().flatten()
                    .chain(by_node.get(&None).into_iter().flatten()).copied();
                classify_children(read.request, &read.expected_spec, candidates, &mut read.observed);
            }
            if !reads.iter().any(|read| matches!(
                read.observed.state, ServiceVpcChainState::Converged | ServiceVpcChainState::Isolated
            )) { return Ok(()); }

            // Discard tentative evidence if Node replacement or set/cluster changes crossed the child read.
            let (nodes, sets, current_cluster) = tokio::try_join!(
                reader.list_nodes(), reader.list_service_chain_sets(&self.namespace),
                read_cluster_identity(&*self.repo, &cluster.namespace, &cluster.name),
            )?;
            let nodes = index_nodes_by_dpu(nodes.items);
            let sets = index_by_name(sets.items, "ServiceChainSet")?;
            for read in &mut reads {
                if !matches!(read.observed.state, ServiceVpcChainState::Converged | ServiceVpcChainState::Isolated) { continue; }
                let initial_set = read.observed.set.clone();
                let valid_snapshot = match validate_remote_snapshot(
                    read.request, &read.expected_spec, &nodes, &sets, &mut read.observed
                ) {
                    Ok(()) => true,
                    Err((ServiceVpcChainState::ReadFailure(error), _)) => {
                        read.observed.read_failed(error);
                        continue;
                    }
                    Err(_) => false,
                };
                let unchanged = current_cluster == *cluster && valid_snapshot
                    && initial_set.as_ref().zip(read.observed.set.as_ref()).is_some_and(|(initial, current)| {
                        initial.uid == current.uid && initial.generation == current.generation
                    });
                if unchanged {
                    // Keep the set observation whose acknowledgement preceded the authoritative child list.
                    read.observed.set = initial_set;
                } else {
                    read.observed.set_state(ServiceVpcChainState::StaleIdentity,
                        Some("Node, ServiceChainSet or DPUCluster changed after the child observation"));
                }
            }
            Ok(())
        }.await;
        if let Err(error) = result {
            let error = Arc::new(error);
            for read in &mut reads {
                read.observed.read_failed(error.clone());
            }
        }
        reads
            .into_iter()
            .map(|read| (read.index, read.observed))
            .collect()
    }
}

/// Reads every registration so an unfiltered current lookup cannot omit a recreated DPUServiceChain.
async fn read_registrations<R: DpuServiceChainRepository>(
    repo: &R,
    namespace: &str,
) -> Result<BTreeMap<String, DPUServiceChain>, DpfError> {
    let resources = bounded_management_request(
        "list service-VPC chain registrations",
        DpuServiceChainRepository::list(repo, namespace, None),
    )
    .await?;
    index_by_name(resources, "DPUServiceChain")
}

/// Indexes complete resource inventories without silently adopting duplicate or unnamed objects.
fn index_by_name<K: Resource>(
    resources: Vec<K>,
    kind: &'static str,
) -> Result<BTreeMap<String, K>, DpfError> {
    let mut by_name = BTreeMap::new();
    for resource in resources {
        let name = resource
            .meta()
            .name
            .as_ref()
            .filter(|name| !name.is_empty())
            .cloned()
            .ok_or_else(|| {
                DpfError::InvalidState(format!("{kind} list contains an unnamed object"))
            })?;
        if by_name.insert(name, resource).is_some() {
            return Err(DpfError::InvalidState(format!(
                "{kind} list contains duplicate names"
            )));
        }
    }
    Ok(by_name)
}

/// Extracts complete Kubernetes identity so successful evidence always carries its exact generation.
fn resource_observation(metadata: &ObjectMeta) -> Option<ServiceVpcResourceObservation> {
    let required =
        |value: &Option<String>| value.as_ref().filter(|value| !value.is_empty()).cloned();
    Some(ServiceVpcResourceObservation {
        namespace: required(&metadata.namespace)?,
        name: required(&metadata.name)?,
        uid: required(&metadata.uid)?,
        resource_version: required(&metadata.resource_version)?,
        generation: metadata.generation.filter(|value| *value > 0)?,
        is_deleting: metadata.deletion_timestamp.is_some(),
    })
}

/// Checks service, interface, slot and selector version against the current management registration.
fn prepare_registration(
    request: &ServiceVpcChainRequest,
    registration: Option<&DPUServiceChain>,
    namespace: &str,
) -> Result<(Value, ServiceVpcResourceObservation), (ServiceVpcChainState, &'static str)> {
    if request.cleanup_bypass_observed {
        return Err((
            ServiceVpcChainState::StaleIdentity,
            "previously observed cleanup bypass requires administrative recovery",
        ));
    }
    let correlation = &request.correlation;
    let definition = &request.definition;
    let identity = DpfHelmChartIdentity::from_service_id(correlation.service_id);
    let labels = [
        (
            DPF_HELM_CHART_OWNER_LABEL,
            correlation.service_id.to_string(),
        ),
        (
            DPF_HELM_CHART_INTERFACE_ORDINAL_LABEL,
            correlation.interface_ordinal.to_string(),
        ),
        (
            DPF_HELM_CHART_SERVICE_VPC_SLOT_LABEL,
            correlation.slot_index.to_string(),
        ),
    ];
    let expected_placement =
        identity.slot_placement_label_key(correlation.interface_ordinal, correlation.slot_index);
    if definition.namespace != namespace
        || definition.name
            != identity.slot_chain_name(correlation.interface_ordinal, correlation.slot_index)
        || definition.selector_version != Some(correlation.selector_version)
        || definition.excluded_dpu_ids.contains(&correlation.dpu_id)
            != (correlation.intent == ServiceVpcChainIntent::Retiring)
        || !labels.iter().all(|(key, value)| {
            definition.labels.get(*key) == Some(value)
                && definition.template_labels.get(*key) == Some(value)
        })
        || definition
            .node_selector_labels
            .get(&expected_placement)
            .map(String::as_str)
            != Some(DPF_HELM_CHART_PLACEMENT_LABEL_VALUE)
        || [
            &request.node.name,
            &request.node.uid,
            &request.node.cluster.name,
            &request.node.cluster.namespace,
            &request.node.cluster.uid,
        ]
        .iter()
        .any(|value| value.is_empty())
    {
        return Err((
            ServiceVpcChainState::StaleIdentity,
            "request does not match its endpoint and selector fields",
        ));
    }
    let pending = || {
        if correlation.intent == ServiceVpcChainIntent::Retiring {
            ServiceVpcChainState::ExclusionPending
        } else {
            ServiceVpcChainState::PendingCreation
        }
    };
    let registration =
        registration.ok_or_else(|| (pending(), "management registration is absent"))?;
    let observed = resource_observation(&registration.metadata).ok_or((
        ServiceVpcChainState::StaleIdentity,
        "management registration identity is incomplete",
    ))?;
    if observed.namespace != namespace
        || observed.name != definition.name
        || !labels.iter().all(|(key, value)| {
            registration
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get(*key))
                == Some(value)
        })
    {
        return Err((
            ServiceVpcChainState::StaleIdentity,
            "management registration ownership differs",
        ));
    }
    if observed.is_deleting {
        return Err((
            ServiceVpcChainState::Deleting,
            "management registration is deleting",
        ));
    }
    let failed = |error| {
        (
            ServiceVpcChainState::ReadFailure(Arc::new(DpfError::SerializationError(error))),
            "registration specification could not be decoded",
        )
    };
    let expected =
        registered_dpu_service_chain_spec(definition, &observed.uid).map_err(|error| {
            (
                ServiceVpcChainState::ReadFailure(Arc::new(error)),
                "registration specification is invalid",
            )
        })?;
    if serde_json::to_value(&registration.spec).map_err(failed)? != expected {
        return Err((
            pending(),
            "management registration has not reached the requested selector version",
        ));
    }
    Ok((expected, observed))
}

/// Requires one true condition for the resource's current generation, rejecting missing or duplicate acknowledgements.
fn current_condition(metadata: &ObjectMeta, conditions: Option<&[Condition]>, name: &str) -> bool {
    let Some(generation) = metadata.generation.filter(|value| *value > 0) else {
        return false;
    };
    let mut matching = conditions
        .unwrap_or_default()
        .iter()
        .filter(|condition| condition.type_ == name);
    matches!((matching.next(), matching.next()), (Some(condition), None)
        if condition.status == "True" && condition.observed_generation == Some(generation))
}

/// Requires declared metadata entries without dropping unrelated labels or annotations.
fn metadata_entries_match(
    observed: Option<&BTreeMap<String, String>>,
    expected: Option<&Value>,
) -> bool {
    match expected {
        None | Some(Value::Null) => true,
        Some(Value::Object(expected)) => expected.iter().all(|(key, value)| {
            value.as_str().is_some_and(|value| {
                observed
                    .and_then(|entries| entries.get(key))
                    .map(String::as_str)
                    == Some(value)
            })
        }),
        _ => false,
    }
}

/// Checks supported template labels and annotations while preserving unrelated controller metadata.
fn template_metadata_matches(metadata: &ObjectMeta, template: Option<&Value>) -> bool {
    let Some(template) = template.and_then(Value::as_object) else {
        return false;
    };
    metadata_entries_match(metadata.labels.as_ref(), template.get("labels"))
        && metadata_entries_match(metadata.annotations.as_ref(), template.get("annotations"))
}

/// Detects missing cleanup protection after a controller has acknowledged reconciliation.
///
/// A deleting object may legitimately lose its finalizer after cleanup. A non-deleting object
/// with historical acknowledgement cannot lose it through the supported controller path.
fn missing_cleanup_protection(
    metadata: &ObjectMeta,
    conditions: Option<&[Condition]>,
    finalizer: &str,
    acknowledgements: &[&str],
) -> bool {
    metadata.deletion_timestamp.is_none()
        && !metadata
            .finalizers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|value| value == finalizer)
        && conditions.unwrap_or_default().iter().any(|condition| {
            acknowledgements.contains(&condition.type_.as_str()) && condition.status == "True"
        })
}

/// Validates Node identity and the exact remote set before child reads or conclusive revalidation.
fn validate_remote_snapshot(
    request: &ServiceVpcChainRequest,
    expected_spec: &Value,
    nodes: &BTreeMap<String, Vec<Node>>,
    sets: &BTreeMap<String, ServiceChainSet>,
    observed: &mut ServiceVpcChainObservation,
) -> Result<(), (ServiceVpcChainState, &'static str)> {
    // Retain detectable cleanup bypass even if other current identity or specification checks fail.
    if let Some(set) = sets.get(&request.definition.name) {
        observed.set = resource_observation(&set.metadata);
        let conditions = set
            .status
            .as_ref()
            .and_then(|status| status.conditions.as_deref());
        if missing_cleanup_protection(
            &set.metadata,
            conditions,
            SET_FINALIZER,
            &["ServiceChainsReconciled"],
        ) {
            observed.cleanup_bypass_observed = true;
            return Err((
                ServiceVpcChainState::StaleIdentity,
                "reconciled ServiceChainSet lacks its cleanup finalizer",
            ));
        }
    }
    let node = observed_node_identity(
        &request.node.cluster,
        nodes,
        request.correlation.dpu_id,
        &request.node.name,
    )
    .map_err(|_| {
        (
            ServiceVpcChainState::StaleIdentity,
            "retained Node is missing, ambiguous or deleting",
        )
    })?;
    observed.node = Some(node.clone());
    if node != request.node {
        return Err((
            ServiceVpcChainState::StaleIdentity,
            "retained Node UID changed",
        ));
    }
    // Require the chain selector's placement labels during activation; retirement tolerates cached labels.
    if request.correlation.intent == ServiceVpcChainIntent::Active
        && nodes
            .get(&request.correlation.dpu_id.to_string())
            .and_then(|nodes| nodes.first())
            .is_none_or(|node| {
                !request
                    .definition
                    .node_selector_labels
                    .iter()
                    .all(|(key, value)| {
                        node.metadata
                            .labels
                            .as_ref()
                            .and_then(|labels| labels.get(key))
                            == Some(value)
                    })
            })
    {
        return Err((
            ServiceVpcChainState::PendingCreation,
            "Node placement has not reached the requested selector",
        ));
    }
    let pending = || {
        if request.correlation.intent == ServiceVpcChainIntent::Retiring {
            ServiceVpcChainState::ExclusionPending
        } else {
            ServiceVpcChainState::PendingCreation
        }
    };
    let set = sets
        .get(&request.definition.name)
        .ok_or_else(|| (pending(), "remote ServiceChainSet is absent"))?;
    observed.set = resource_observation(&set.metadata);
    let identity = observed.set.as_ref().ok_or((
        ServiceVpcChainState::StaleIdentity,
        "ServiceChainSet identity is incomplete",
    ))?;
    if identity.namespace != request.definition.namespace
        || identity.name != request.definition.name
        || !template_metadata_matches(&set.metadata, expected_spec.pointer("/template/metadata"))
    {
        return Err((
            ServiceVpcChainState::StaleIdentity,
            "ServiceChainSet registration identity differs",
        ));
    }
    if identity.is_deleting {
        return Err((
            ServiceVpcChainState::Deleting,
            "ServiceChainSet is deleting",
        ));
    }
    let spec = serde_json::to_value(&set.spec).map_err(|error| {
        (
            ServiceVpcChainState::ReadFailure(Arc::new(error.into())),
            "ServiceChainSet specification could not be decoded",
        )
    })?;
    if expected_spec.pointer("/template/spec") != Some(&spec) {
        return Err((
            pending(),
            "ServiceChainSet has not reached the requested specification",
        ));
    }
    let conditions = set
        .status
        .as_ref()
        .and_then(|status| status.conditions.as_deref());
    let acknowledged = current_condition(&set.metadata, conditions, "ServiceChainsReconciled");
    if !set
        .metadata
        .finalizers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|value| value == SET_FINALIZER)
    {
        return Err((
            pending(),
            "ServiceChainSet cleanup finalizer has not been installed",
        ));
    }
    if request.correlation.intent == ServiceVpcChainIntent::Retiring && !acknowledged {
        return Err((
            ServiceVpcChainState::ExclusionPending,
            "ServiceChainsReconciled has not acknowledged the current set generation",
        ));
    }
    Ok(())
}

/// Keeps mismatched ancestry and interface references visible so labels alone cannot establish absence.
fn relevant_child(
    child: &ServiceChain,
    request: &ServiceVpcChainRequest,
    observed: &ServiceVpcChainObservation,
) -> bool {
    let labels = child.metadata.labels.as_ref();
    let label = |key: &str| {
        labels
            .and_then(|labels| labels.get(key))
            .map(String::as_str)
    };
    let tracked_set = label(SET_NAME_LABEL) == Some(request.definition.name.as_str())
        && label(SET_NAMESPACE_LABEL) == Some(request.definition.namespace.as_str());
    let current_registration = observed.registration.as_ref().is_some_and(|registration| {
        label(SERVICE_VPC_CHAIN_REGISTRATION_UID_LABEL) == Some(registration.uid.as_str())
    });
    let owned_set = observed.set.as_ref().is_some_and(|set| {
        child
            .metadata
            .owner_references
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|owner| {
                owner.uid == set.uid
                    || (owner.name == set.name
                        && owner.kind == "ServiceChainSet"
                        && owner.api_version == "svc.dpu.nvidia.com/v1alpha1")
            })
    });
    let endpoint_labels = [
        (
            DPF_HELM_CHART_OWNER_LABEL,
            request.correlation.service_id.to_string(),
        ),
        (
            DPF_HELM_CHART_INTERFACE_ORDINAL_LABEL,
            request.correlation.interface_ordinal.to_string(),
        ),
        (
            DPF_HELM_CHART_SERVICE_VPC_SLOT_LABEL,
            request.correlation.slot_index.to_string(),
        ),
    ]
    .iter()
    .all(|(key, value)| label(key) == Some(value.as_str()));
    // The service-side interface is shared across slots; only this slot's patch connects its VPC.
    let slot_label = DpfHelmChartIdentity::from_service_id(request.correlation.service_id)
        .slot_placement_label_key(
            request.correlation.interface_ordinal,
            request.correlation.slot_index,
        );
    let connected_slot = child
        .spec
        .switches
        .iter()
        .flat_map(|switch| &switch.ports)
        .any(|port| {
            port.service_interface
                .match_labels
                .get(&slot_label)
                .map(String::as_str)
                == Some(DPF_HELM_CHART_PLACEMENT_LABEL_VALUE)
        });
    tracked_set || current_registration || owned_set || endpoint_labels || connected_slot
}

/// Requires one controller reference to the observed set and an exact target Node.
fn child_has_expected_ancestry(
    child: &ServiceChain,
    request: &ServiceVpcChainRequest,
    set: Option<&ServiceVpcResourceObservation>,
) -> bool {
    let mut controllers = child
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|owner| owner.controller == Some(true));
    child.metadata.namespace.as_deref() == Some(request.definition.namespace.as_str())
        && child.spec.node.as_deref() == Some(request.node.name.as_str())
        && matches!((controllers.next(), controllers.next()), (Some(owner), None)
            if set.is_some_and(|set| owner.uid == set.uid && owner.name == set.name)
            && owner.kind == "ServiceChainSet" && owner.api_version == "svc.dpu.nvidia.com/v1alpha1")
}

/// Classifies fresh children for one retained Node without treating deletion or stale readiness as completion.
fn classify_children<'a>(
    request: &ServiceVpcChainRequest,
    expected_spec: &Value,
    children: impl Iterator<Item = &'a ServiceChain>,
    observed: &mut ServiceVpcChainObservation,
) {
    let relevant = children
        .filter(|child| relevant_child(child, request, observed))
        .collect::<Vec<_>>();
    let mut identities = Vec::with_capacity(relevant.len());
    for child in &relevant {
        let Some(identity) = resource_observation(&child.metadata) else {
            observed.set_state(
                ServiceVpcChainState::StaleIdentity,
                Some("relevant ServiceChain identity is incomplete"),
            );
            return;
        };
        identities.push(identity);
    }
    identities.sort_by(|left, right| (&left.name, &left.uid).cmp(&(&right.name, &right.uid)));
    observed.chains = identities;
    for child in &relevant {
        let conditions = child
            .status
            .as_ref()
            .and_then(|status| status.conditions.as_deref());
        if missing_cleanup_protection(
            &child.metadata,
            conditions,
            CHAIN_FINALIZER,
            &["ServiceChainReconciled", "Ready"],
        ) {
            observed.cleanup_bypass_observed = true;
            observed.set_state(
                ServiceVpcChainState::StaleIdentity,
                Some("reconciled ServiceChain lacks its cleanup finalizer"),
            );
            return;
        }
    }
    if relevant
        .iter()
        .any(|child| child.metadata.deletion_timestamp.is_some())
    {
        observed.set_state(
            ServiceVpcChainState::Deleting,
            Some("relevant ServiceChain deletion is in progress"),
        );
        return;
    }
    if request.correlation.intent == ServiceVpcChainIntent::Retiring {
        // Older selector-version annotations are normal during withdrawal; ancestry and registration labels are not optional.
        let expected_labels = expected_spec.pointer("/template/spec/template/metadata/labels");
        if !relevant.iter().all(|child| {
            child_has_expected_ancestry(child, request, observed.set.as_ref())
                && metadata_entries_match(child.metadata.labels.as_ref(), expected_labels)
        }) {
            observed.set_state(
                ServiceVpcChainState::StaleIdentity,
                Some("withdrawal has a mismatched-identity ServiceChain leftover"),
            );
        } else {
            observed.set_state(
                if relevant.is_empty() {
                    ServiceVpcChainState::Isolated
                } else {
                    ServiceVpcChainState::Deleting
                },
                (!relevant.is_empty()).then_some(
                    "relevant ServiceChains still remain after exclusion acknowledgement",
                ),
            );
        }
        return;
    }
    let [child] = relevant.as_slice() else {
        observed.set_state(
            if relevant.is_empty() {
                ServiceVpcChainState::PendingCreation
            } else {
                ServiceVpcChainState::StaleIdentity
            },
            Some("active endpoint does not identify exactly one ServiceChain"),
        );
        return;
    };

    // Require immutable ancestry as well as template stamps; an old set can leave same-label children.
    let switches = match serde_json::to_value(&child.spec.switches) {
        Ok(switches) => switches,
        Err(error) => {
            observed.read_failed(Arc::new(error.into()));
            return;
        }
    };
    if !child_has_expected_ancestry(child, request, observed.set.as_ref())
        || !template_metadata_matches(
            &child.metadata,
            expected_spec.pointer("/template/spec/template/metadata"),
        )
        || expected_spec.pointer("/template/spec/template/spec/switches") != Some(&switches)
    {
        observed.set_state(
            ServiceVpcChainState::StaleIdentity,
            Some("ServiceChain ancestry, template identity or specification differs"),
        );
        return;
    }
    let conditions = child
        .status
        .as_ref()
        .and_then(|status| status.conditions.as_deref());
    let reconciled = current_condition(&child.metadata, conditions, "ServiceChainReconciled");
    let ready = current_condition(&child.metadata, conditions, "Ready");
    if !child
        .metadata
        .finalizers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|value| value == CHAIN_FINALIZER)
    {
        // Historical acknowledgement without protection was rejected above; this child has not reconciled yet.
        observed.set_state(
            ServiceVpcChainState::PendingCreation,
            Some("ServiceChain cleanup finalizer is absent"),
        );
    } else if reconciled && ready {
        observed.set_state(ServiceVpcChainState::Converged, None);
    } else {
        observed.set_state(
            ServiceVpcChainState::PendingCreation,
            Some("ServiceChain conditions have not acknowledged the current generation"),
        );
    }
}

#[cfg(test)]
mod tests;
