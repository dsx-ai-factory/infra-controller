// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::mem::discriminant;
use std::time::Duration;

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use config_version::ConfigVersion;
use futures::FutureExt as _;
use http::Response;
use k8s_openapi::api::core::v1::Node;
use kube::client::Body;
use model::extension_service::{
    DPF_HELM_CHART_INTERFACE_ORDINAL_LABEL, DPF_HELM_CHART_OWNER_LABEL,
    DPF_HELM_CHART_PLACEMENT_LABEL_VALUE, DPF_HELM_CHART_SERVICE_VPC_SLOT_LABEL,
    DpfHelmChartIdentity,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::{ApiHandle, FIRST_DPU, SECOND_DPU, THIRD_DPU, add_dpu, add_reader, fixture};
use crate::crds::dpuservicechains_generated::DPUServiceChain;
use crate::crds::servicechains_generated::ServiceChain;
use crate::crds::servicechainsets_generated::ServiceChainSet;
use crate::sdk::tests::{SdkMock, TEST_NAMESPACE};
use crate::sdk::{DpfSdk, registered_dpu_service_chain_spec};
use crate::types::{
    DetachedDpuServiceChainDefinition, DpuClusterIdentity, ServiceVpcChainIntent,
    ServiceVpcChainObservation, ServiceVpcChainRequest, ServiceVpcChainRequestContext,
    ServiceVpcChainState, ServiceVpcNodeIdentity,
};
use crate::{DPU_MACHINE_ID_LABEL, DpfError, SERVICE_VPC_MTU};

/// Supplies one committed endpoint and retained Node identity so tests vary only the desired chain phase.
pub(in crate::sdk) fn request(intent: ServiceVpcChainIntent) -> ServiceVpcChainRequest {
    let service_id = "00000000-0000-0000-0000-000000000017"
        .parse()
        .expect("service ID");
    let dpu_id = FIRST_DPU.parse().expect("DPU ID");
    let selector_version = ConfigVersion::initial();
    let identity = DpfHelmChartIdentity::from_service_id(service_id);
    let labels = BTreeMap::from([
        (
            DPF_HELM_CHART_OWNER_LABEL.to_owned(),
            service_id.to_string(),
        ),
        (
            DPF_HELM_CHART_INTERFACE_ORDINAL_LABEL.to_owned(),
            "0".to_owned(),
        ),
        (
            DPF_HELM_CHART_SERVICE_VPC_SLOT_LABEL.to_owned(),
            "0".to_owned(),
        ),
    ]);
    let generic = BTreeMap::from([(
        identity.placement_label_key.clone(),
        DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned(),
    )]);
    let slot = BTreeMap::from([(
        identity.slot_placement_label_key(0, 0),
        DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned(),
    )]);
    ServiceVpcChainRequest {
        request_context: ServiceVpcChainRequestContext {
            attachment_id: uuid::Uuid::new_v4(),
            internal_uuid: uuid::Uuid::new_v4(),
            service_id,
            interface_ordinal: 0,
            slot_index: 0,
            dpu_id,
            extension_services_config_version: ConfigVersion::initial(),
            network_config_version: ConfigVersion::initial(),
            selector_version,
            intent,
        },
        definition: DetachedDpuServiceChainDefinition {
            name: identity.slot_chain_name(0, 0),
            namespace: TEST_NAMESPACE.to_owned(),
            labels: labels.clone(),
            node_selector_labels: slot.clone(),
            // Match retirement intent to the committed exclusion required by registration validation.
            excluded_dpu_ids: if intent == ServiceVpcChainIntent::Retiring {
                vec![dpu_id]
            } else {
                vec![]
            },
            selector_version: Some(selector_version),
            template_labels: labels,
            interface_selectors: vec![generic, slot],
            service_mtu: SERVICE_VPC_MTU,
        },
        node: ServiceVpcNodeIdentity {
            cluster: DpuClusterIdentity {
                namespace: TEST_NAMESPACE.to_owned(),
                name: "cluster-a".to_owned(),
                uid: "cluster-a-uid".to_owned(),
            },
            name: "first-node".to_owned(),
            uid: "first-node-uid".to_owned(),
        },
        cleanup_bypass_observed: false,
    }
}

/// Supplies one condition with its own observedGeneration so fixtures do not rely on top-level generation.
fn condition(name: &str, generation: i64, status: &str) -> Value {
    json!({
        "type": name, "status": status, "observedGeneration": generation,
        "reason": "Reconciled", "message": "", "lastTransitionTime": "2026-11-06T00:00:00Z",
    })
}

/// Supplies the exact UID-stamped management registration used by the SDK's canonical comparison.
pub(in crate::sdk) fn registration(request: &ServiceVpcChainRequest) -> DPUServiceChain {
    serde_json::from_value(json!({
        "metadata": {
            "name": request.definition.name, "namespace": TEST_NAMESPACE, "uid": "registration-uid",
            "generation": 4, "resourceVersion": "4", "labels": request.definition.labels,
        },
        "spec": registered_dpu_service_chain_spec(&request.definition, "registration-uid").expect("registered spec"),
    })).expect("registration fixture")
}

/// Supplies a current acknowledged set whose aggregate Ready remains false so tests require per-Node child evidence.
pub(in crate::sdk) fn set(request: &ServiceVpcChainRequest) -> ServiceChainSet {
    let spec = registered_dpu_service_chain_spec(&request.definition, "registration-uid")
        .expect("registered spec");
    serde_json::from_value(json!({
        "metadata": {
            "name": request.definition.name, "namespace": TEST_NAMESPACE, "uid": "set-uid",
            "generation": 3, "resourceVersion": "11", "finalizers": ["dpu.nvidia.com/servicechainset"],
            "labels": spec["template"]["metadata"]["labels"], "annotations": spec["template"]["metadata"]["annotations"],
        },
        "spec": spec["template"]["spec"],
        "status": {"observedGeneration": 3, "conditions": [
            condition("ServiceChainsReconciled", 3, "True"),
            condition("Ready", 3, "False"),
        ]},
    })).expect("set fixture")
}

/// Supplies a non-deleting child with current ancestry, template stamps and both required acknowledgements
/// so cases can vary one prerequisite without unrelated missing evidence.
pub(in crate::sdk) fn child(request: &ServiceVpcChainRequest) -> ServiceChain {
    let spec = registered_dpu_service_chain_spec(&request.definition, "registration-uid")
        .expect("registered spec");
    let template = &spec["template"]["spec"]["template"];
    // Bind the shared template to the retained Node before cases alter any ancestry evidence.
    let mut child_spec = template["spec"].clone();
    child_spec["node"] = json!(request.node.name);
    serde_json::from_value(json!({
        "metadata": {
            "name": "current-child", "namespace": TEST_NAMESPACE, "uid": "child-uid",
            "generation": 2, "resourceVersion": "13",
            "labels": template["metadata"]["labels"], "annotations": template["metadata"]["annotations"],
            "finalizers": ["svc.dpu.nvidia.com/ServiceChain-finalizer"],
            "ownerReferences": [{
                "apiVersion": "svc.dpu.nvidia.com/v1alpha1", "kind": "ServiceChainSet",
                "name": request.definition.name, "uid": "set-uid", "controller": true,
            }],
        },
        "spec": child_spec,
        "status": {"observedGeneration": 2, "conditions": [
            condition("ServiceChainReconciled", 2, "True"), condition("Ready", 2, "True"),
        ]},
    })).expect("child fixture")
}

/// Supplies the retained Node with placement labels during retirement so tests can prove isolation tolerates cached placement.
pub(in crate::sdk) fn node(request: &ServiceVpcChainRequest) -> Node {
    let mut labels = request.definition.node_selector_labels.clone();
    labels.insert(
        DPU_MACHINE_ID_LABEL.to_owned(),
        request.request_context.dpu_id.to_string(),
    );
    serde_json::from_value(json!({
        "metadata": {"name": request.node.name, "uid": request.node.uid, "labels": labels},
    }))
    .expect("Node fixture")
}

/// Encodes complete authoritative list metadata so protocol tests exercise ordering and identity.
fn list_response<T: Serialize>(items: &[T], resource_version: &str) -> Response<Body> {
    Response::builder()
        .status(200)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "apiVersion": "v1", "kind": "List",
                "metadata": {"resourceVersion": resource_version}, "items": items,
            })
            .to_string()
            .into_bytes(),
        ))
        .expect("list response")
}

/// Answers Nodes and holds the set acknowledgement until proving no child list has been issued.
async fn acknowledge_snapshot(handle: &mut ApiHandle, request: &ServiceVpcChainRequest) {
    // Accept concurrent Node and set reads in either order while withholding the set response.
    let mut acknowledgement = None;
    for _ in 0..2 {
        let (call, sender) = handle
            .next_request()
            .await
            .expect("initial snapshot request");
        assert_eq!(call.method(), http::Method::GET);
        if call.uri().path() == "/api/v1/nodes" {
            sender.send_response(list_response(&[node(request)], "10"));
        } else {
            assert_eq!(
                call.uri().path(),
                format!(
                    "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechainsets"
                )
            );
            acknowledgement = Some(sender);
        }
    }

    // Let client tasks dispatch any prematurely started read before checking the held acknowledgement.
    tokio::task::yield_now().await;
    // A child list obtained before this response would not be ordered after the acknowledgement.
    assert!(
        handle.next_request().now_or_never().is_none(),
        "child list preceded set acknowledgement"
    );
    // Release the acknowledgement only after checking that no child read preceded its response.
    acknowledgement
        .expect("set request")
        .send_response(list_response(&[set(request)], "11"));
}

/// Identifies one independently mutable identity or generation checked after the child observation.
#[derive(Clone, Copy)]
enum ChangedIdentity {
    NodeUid,
    SetUid,
    SetGeneration,
    RegistrationUid,
    RegistrationGeneration,
}

/// Answers final Node and set reads, applying any selected Node or set change to expose stale evidence.
async fn revalidate_snapshot(
    handle: &mut ApiHandle,
    request: &ServiceVpcChainRequest,
    changed: Option<ChangedIdentity>,
) {
    // Accept concurrent rereads in either order so the mock imposes no extra ordering requirement.
    for _ in 0..2 {
        let (call, sender) = handle.next_request().await.expect("revalidation request");
        assert_eq!(call.method(), http::Method::GET);
        if call.uri().path() == "/api/v1/nodes" {
            let mut current = node(request);
            if matches!(changed, Some(ChangedIdentity::NodeUid)) {
                // A replacement can retain the same name and machine label while changing UID.
                current.metadata.uid = Some("replacement-node-uid".to_owned());
            }
            sender.send_response(list_response(&[current], "21"));
        } else {
            assert_eq!(
                call.uri().path(),
                format!(
                    "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechainsets"
                )
            );
            let mut current = set(request);
            // Status writes may change resourceVersion without invalidating the acknowledged spec.
            current.metadata.resource_version = Some("22".to_owned());
            match changed {
                Some(ChangedIdentity::SetUid) => {
                    // Keep generation and desired spec fixed to isolate the immutable-UID check.
                    current.metadata.uid = Some("replacement-set-uid".to_owned());
                }
                Some(ChangedIdentity::SetGeneration) => {
                    // Model a selector edit and restoration, with a new current acknowledgement.
                    current.metadata.generation = Some(5);
                    for condition in current
                        .status
                        .as_mut()
                        .expect("status")
                        .conditions
                        .as_mut()
                        .expect("conditions")
                    {
                        condition.observed_generation = Some(5);
                    }
                }
                _ => {}
            }
            sender.send_response(list_response(&[current], "22"));
        }
    }
}

/// Seeds the existing SDK mock with the observed registration so remote tests exercise final rereads.
fn insert_registration(repo: &SdkMock, request: &ServiceVpcChainRequest) {
    let registration = registration(request);
    repo.service_chains
        .write()
        .expect("chains")
        .insert(SdkMock::key(&registration), registration);
}

/// Serves complete inventories through optional final rereads so incorrect conclusive states reach assertions.
async fn observe_children(
    sdk: &DpfSdk<SdkMock>,
    requests: &[ServiceVpcChainRequest],
    handle: &mut ApiHandle,
    nodes: &[Node],
    sets: &[ServiceChainSet],
    children: &[Value],
) -> (
    Vec<ServiceVpcChainObservation>,
    BTreeMap<&'static str, usize>,
) {
    let sets_path =
        format!("/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechainsets");
    let children_path =
        format!("/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechains");
    let mut read_counts = BTreeMap::new();

    // Keep answering revalidation if a regression wrongly classifies a leftover as successful absence.
    let observations = tokio::time::timeout(Duration::from_secs(5), async {
        let observation = sdk.observe_service_vpc_chains(requests);
        tokio::pin!(observation);
        loop {
            tokio::select! {
                observations = &mut observation => break observations,
                call = handle.next_request() => {
                    let (call, sender) = call.expect("remote observation request");
                    assert_eq!(call.method(), http::Method::GET);
                    let (kind, response) = match call.uri().path() {
                        "/api/v1/nodes" => ("nodes", list_response(nodes, "10")),
                        path if path == sets_path => ("sets", list_response(sets, "11")),
                        path if path == children_path => ("chains", list_response(children, "20")),
                        path => panic!("unexpected observation path: {path}"),
                    };
                    *read_counts.entry(kind).or_default() += 1;
                    sender.send_response(response);
                }
            }
        }
    })
    .await
    .expect("chain observation must finish");
    (observations, read_counts)
}

/// Preserves interleaved request context while sharing cluster reads and isolating a failed cluster,
/// preventing one endpoint's withdrawal evidence from being assigned to another endpoint.
#[tokio::test]
async fn chain_observations_batch_requests_and_isolate_cluster_failures() {
    // Two distinct Nodes share one registration; the third Node belongs to a cluster without a reader.
    let isolated = request(ServiceVpcChainIntent::Retiring);
    // Pair the sibling DPU with its own retained Node while preserving the shared attachment and selector.
    let mut draining = isolated.clone();
    draining.request_context.dpu_id = SECOND_DPU.parse().expect("second DPU ID");
    draining.node.name = "second-node".to_owned();
    draining.node.uid = "second-node-uid".to_owned();
    // Retain a valid cluster identity while leaving its reader unavailable for this observation.
    let mut unavailable = isolated.clone();
    unavailable.request_context.dpu_id = THIRD_DPU.parse().expect("third DPU ID");
    unavailable.node.name = "third-node".to_owned();
    unavailable.node.uid = "third-node-uid".to_owned();
    unavailable.node.cluster.name = "cluster-b".to_owned();
    unavailable.node.cluster.uid = "cluster-b-uid".to_owned();

    // Interleave clusters so returning grouped results changes input order regardless of completion order.
    let mut requests = [isolated, unavailable, draining];
    // Use one committed selector excluding every requested DPU so no request fails definition consistency.
    let excluded_dpu_ids = requests
        .iter()
        .map(|request| request.request_context.dpu_id)
        .collect::<Vec<_>>();
    for request in &mut requests {
        request.definition.excluded_dpu_ids = excluded_dpu_ids.clone();
    }
    let (sdk, repo) = fixture();
    add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
    add_dpu(&repo, "second", SECOND_DPU, "cluster-a");
    add_dpu(&repo, "third", THIRD_DPU, "cluster-b");
    insert_registration(&repo, &requests[0]);
    let mut handle = add_reader(&repo, "cluster-a");
    let nodes = [node(&requests[0]), node(&requests[2])];
    let sets = [set(&requests[0])];
    // Only the second Node retains a child in the shared inventory.
    let children = [serde_json::to_value(child(&requests[2])).expect("draining child")];

    // Observe both clusters through the public operation, serving shared snapshots for the available cluster.
    let (observations, read_counts) =
        observe_children(&sdk, &requests, &mut handle, &nodes, &sets, &children).await;

    // Full request context and retained identities must stay attached to the input endpoint after regrouping.
    assert_eq!(
        observations
            .iter()
            .map(|observed| &observed.request_context)
            .collect::<Vec<_>>(),
        requests
            .iter()
            .map(|request| &request.request_context)
            .collect::<Vec<_>>()
    );
    let [isolated, unavailable, draining] = observations.as_slice() else {
        panic!("one result per request");
    };
    // Successful absence retains this Node and the resourceVersion of the child list used to establish isolation.
    assert!(matches!(isolated.state, ServiceVpcChainState::Isolated));
    assert_eq!(isolated.node.as_ref(), Some(&requests[0].node));
    assert!(isolated.chains.is_empty());
    assert_eq!(isolated.chain_list_resource_version.as_deref(), Some("20"));
    // An unavailable reader fails only its cluster and supplies no remote absence evidence.
    assert!(matches!(
        &unavailable.state,
        ServiceVpcChainState::ReadFailure(error)
            if matches!(error.as_ref(), DpfError::InvalidState(message) if message == "observer client is unavailable")
    ));
    assert!(unavailable.node.is_none());
    assert!(unavailable.chains.is_empty());
    assert!(unavailable.chain_list_resource_version.is_none());
    // The sibling Node keeps its own remaining child and cannot inherit the first Node's isolation.
    assert!(matches!(draining.state, ServiceVpcChainState::Deleting));
    assert_eq!(draining.node.as_ref(), Some(&requests[2].node));
    let [remaining] = draining.chains.as_slice() else {
        panic!("one draining child");
    };
    assert_eq!(
        (remaining.name.as_str(), remaining.uid.as_str()),
        ("current-child", "child-uid")
    );
    assert_eq!(draining.chain_list_resource_version.as_deref(), Some("20"));

    // Siblings share one reader and child list, with one initial and one final Node/set snapshot.
    assert_eq!(
        read_counts,
        BTreeMap::from([("nodes", 2), ("sets", 2), ("chains", 1)])
    );
    // Attempt to open each distinct cluster once, including the unavailable reader,
    // while batching sibling requests.
    let mut opened_clusters = repo
        .observer_requests
        .read()
        .expect("observer requests")
        .clone();
    opened_clusters.sort();
    assert_eq!(
        opened_clusters,
        vec![
            requests[0].node.cluster.clone(),
            requests[1].node.cluster.clone()
        ]
    );
    assert!(
        handle.next_request().now_or_never().is_none(),
        "unexpected extra remote read"
    );
}

/// Returns Converged for a current Active child and StaleIdentity when the Node UID or registration
/// generation changes across the child read, preventing callers from consuming stale activation evidence.
#[tokio::test]
async fn activation_revalidates_node_and_registration_after_child_observation() {
    let cases = [
        // Current child evidence must pass public dispatch and both revalidation stages.
        (
            "current Active child",
            None,
            ServiceVpcChainState::Converged,
        ),
        // Same-name Node replacement invalidates the remote evidence even when the child was current.
        (
            "Node UID changed",
            Some(ChangedIdentity::NodeUid),
            ServiceVpcChainState::StaleIdentity,
        ),
        // Restoring the same management spec cannot restore the generation observed before the child read.
        (
            "registration generation changed",
            Some(ChangedIdentity::RegistrationGeneration),
            ServiceVpcChainState::StaleIdentity,
        ),
    ];
    check_cases_async(
        cases.map(|(scenario, changed, expected)| Case {
            scenario,
            input: changed,
            expect: Yields(discriminant(&expected)),
        }),
        |changed| async move {
            // Vary the identity crossing; the selector, registration, Node and child otherwise describe a current activation.
            let request = request(ServiceVpcChainIntent::Active);
            let (sdk, repo) = fixture();
            add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
            insert_registration(&repo, &request);
            let mut handle = add_reader(&repo, "cluster-a");
            let requests = [request.clone()];
            let sets_path = format!(
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechainsets"
            );
            let children_path = format!(
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechains"
            );
            let mut read_counts = BTreeMap::<_, usize>::new();

            // Complete the responder when the SDK returns so skipped revalidation reaches the result checks.
            let observations = tokio::time::timeout(Duration::from_secs(5), async {
                let observation = sdk.observe_service_vpc_chains(&requests);
                tokio::pin!(observation);
                loop {
                    tokio::select! {
                        observations = &mut observation => break observations,
                        call = handle.next_request() => {
                            let (call, sender) = call.expect("Active observation request");
                            assert_eq!(call.method(), http::Method::GET);
                            // Use opaque resourceVersion markers (numeric spelling does not imply ordering).
                            let (kind, response) = match call.uri().path() {
                                // Serve the initial and final Node reads with the arbitrary list marker "21".
                                "/api/v1/nodes" => {
                                    let mut current = node(&request);
                                    if read_counts.contains_key("chains")
                                        && matches!(changed, Some(ChangedIdentity::NodeUid))
                                    {
                                        // Preserve name and machine label while replacing the Node after the child read.
                                        current.metadata.uid = Some("replacement-node-uid".to_owned());
                                    }
                                    ("nodes", list_response(&[current], "21"))
                                }
                                // Serve both set reads with the same UID, generation and spec.
                                path if path == sets_path => {
                                    let mut current = set(&request);
                                    // Advance only resourceVersion on the final read to distinguish the retained pre-child set.
                                    // Use "11" before the child read and "22" on the final read.
                                    let resource_version = if read_counts.contains_key("chains") { "22" } else { "11" };
                                    current.metadata.resource_version = Some(resource_version.to_owned());
                                    ("sets", list_response(&[current], resource_version))
                                }
                                // Serve the current child with "20" as the complete child-list snapshot marker.
                                path if path == children_path => {
                                    if matches!(changed, Some(ChangedIdentity::RegistrationGeneration)) {
                                        // Model accepted spec edits that restore the desired spec while advancing its generation
                                        // from 4 to 6 (one edit and its revert).
                                        let mut registrations = repo.service_chains.write().expect("chains");
                                        let current = registrations.get_mut(&SdkMock::ns_key(TEST_NAMESPACE, &request.definition.name))
                                            .expect("current registration");
                                        current.metadata.generation = Some(6);
                                        // Use "6" as an opaque observation marker distinct from the baseline "4".
                                        current.metadata.resource_version = Some("6".to_owned());
                                    }
                                    ("chains", list_response(&[child(&request)], "20"))
                                }
                                // Fail on unexpected paths so the responder cannot supply unrelated observation evidence.
                                path => panic!("unexpected observation path: {path}"),
                            };
                            *read_counts.entry(kind).or_default() += 1;
                            sender.send_response(response);
                        }
                    }
                }
            })
            .await
            .expect("Active observation must finish");

            // Every outcome must stay attached to the complete caller request context.
            let [observed] = observations.as_slice() else {
                panic!("one result per request");
            };
            assert_eq!(observed.request_context, request.request_context);
            if changed.is_none() && matches!(observed.state, ServiceVpcChainState::Converged) {
                // Successful activation retains the original identities and complete child-list evidence.
                assert_eq!(observed.node.as_ref(), Some(&request.node));
                let registration = observed.registration.as_ref().expect("registration identity");
                assert_eq!(
                    (registration.uid.as_str(), registration.generation, registration.resource_version.as_str()),
                    ("registration-uid", 4, "4")
                );
                let set = observed.set.as_ref().expect("set identity");
                assert_eq!(
                    (set.uid.as_str(), set.generation, set.resource_version.as_str()),
                    ("set-uid", 3, "11")
                );
                let [converged_child] = observed.chains.as_slice() else {
                    panic!("one current child");
                };
                assert_eq!(
                    (converged_child.name.as_str(), converged_child.uid.as_str(), converged_child.generation, converged_child.resource_version.as_str()),
                    ("current-child", "child-uid", 2, "13")
                );
                assert_eq!(observed.chain_list_resource_version.as_deref(), Some("20"));
                assert!(!observed.cleanup_bypass_observed);
                // Both remote snapshots must be read even when the first child observation is conclusive.
                assert_eq!(
                    read_counts,
                    BTreeMap::from([("nodes", 2), ("sets", 2), ("chains", 1)])
                );
            }
            Ok::<_, ()>(discriminant(&observed.state))
        },
    )
    .await;
}

/// Retains a relevant child with omitted or empty spec.node after exclusion acknowledgement,
/// preventing the public candidate index from turning uncertain Node ancestry into isolation.
#[tokio::test]
async fn withdrawal_retains_relevant_children_without_node_identity() {
    let cases = [
        // Omitted spec.node must retain the relevant child even without an exact Node index entry.
        ("omitted spec.node", None),
        // Empty spec.node must normalize alongside omitted values so indexing retains the child.
        ("empty spec.node", Some("")),
    ];
    check_cases_async(
        cases.map(|(scenario, missing_node)| Case {
            scenario,
            input: missing_node,
            expect: Yields(()),
        }),
        |missing_node| async move {
            // Vary only spec.node; owner references, template stamps, finalizers and conditions otherwise match.
            let request = request(ServiceVpcChainIntent::Retiring);
            let mut leftover = serde_json::to_value(child(&request)).expect("leftover child");
            if let Some(name) = missing_node {
                leftover["spec"]["node"] = json!(name);
            } else {
                // Omit the property because the CRD allows absence or a string, rather than null.
                leftover["spec"]
                    .as_object_mut()
                    .expect("child spec")
                    .remove("node")
                    .expect("child Node");
            }
            let (sdk, repo) = fixture();
            add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
            insert_registration(&repo, &request);
            let mut handle = add_reader(&repo, "cluster-a");
            let requests = [request.clone()];

            // Pass the leftover through public indexing and permit final rereads if it is wrongly dropped.
            let (observations, read_counts) = observe_children(
                &sdk,
                &requests,
                &mut handle,
                &[node(&request)],
                &[set(&request)],
                &[leftover],
            )
            .await;

            // Unknown Node ancestry blocks isolation while preserving the observed child and complete-list identity.
            let [observed] = observations.as_slice() else {
                panic!("one result per request");
            };
            assert!(
                matches!(observed.state, ServiceVpcChainState::StaleIdentity),
                "{observed:?}"
            );
            assert_eq!(observed.request_context, request.request_context);
            assert_eq!(observed.node.as_ref(), Some(&request.node));
            let [remaining] = observed.chains.as_slice() else {
                panic!("one relevant leftover");
            };
            assert_eq!(
                (remaining.name.as_str(), remaining.uid.as_str()),
                ("current-child", "child-uid")
            );
            assert_eq!(observed.chain_list_resource_version.as_deref(), Some("20"));
            assert_eq!(observed.set.as_ref().expect("set identity").uid, "set-uid");
            assert_eq!(
                observed
                    .registration
                    .as_ref()
                    .expect("registration identity")
                    .uid,
                "registration-uid"
            );
            // Retaining a child with omitted or empty spec.node must stop isolation
            // before final Node and set revalidation reads.
            assert_eq!(
                read_counts,
                BTreeMap::from([("nodes", 1), ("sets", 1), ("chains", 1)])
            );
            Ok::<_, ()>(())
        },
    )
    .await;
}

/// Establishes target isolation only after acknowledgement and fresh absence, while preserving a
/// sibling Node's chain and tolerating cached placement and false aggregate Ready.
#[tokio::test]
async fn withdrawal_lists_children_after_acknowledgement_and_scopes_absence_to_the_node() {
    // Retain a known Node identity with no prior local child; another Node still has a service chain.
    let request = request(ServiceVpcChainIntent::Retiring);
    let (sdk, repo) = fixture();
    add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
    insert_registration(&repo, &request);
    let mut handle = add_reader(&repo, "cluster-a");
    let requests = [request.clone()];

    // Hold the acknowledgement, then supply a fresh list containing only the sibling's child.
    let (observations, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(sdk.observe_service_vpc_chains(&requests), async {
            acknowledge_snapshot(&mut handle, &request).await;
            let (call, sender) = handle.next_request().await.expect("post-ack child list");
            assert_eq!(call.method(), http::Method::GET);
            assert_eq!(
                call.uri().path(),
                format!(
                    "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechains"
                )
            );
            // Retain the slot and set ancestry while placing the child on another Node to scope absence.
            let mut sibling = child(&request);
            sibling.metadata.name = Some("sibling-child".to_owned());
            sibling.metadata.uid = Some("sibling-child-uid".to_owned());
            sibling.spec.node = Some("sibling-node".to_owned());
            // Deliver the post-acknowledgement child list before answering final Node and set rereads.
            sender.send_response(list_response(&[sibling], "20"));
            revalidate_snapshot(&mut handle, &request, None).await;
        })
    })
    .await
    .expect("ordered withdrawal observation must finish");

    // The complete list need not be empty globally; only this retained Node must have no relevant child.
    let [observed] = observations.as_slice() else {
        panic!("one result per request");
    };
    assert!(
        matches!(observed.state, ServiceVpcChainState::Isolated),
        "{observed:?}"
    );
    assert_eq!(observed.request_context, request.request_context);
    assert_eq!(observed.node.as_ref(), Some(&request.node));
    assert!(observed.chains.is_empty());
    assert_eq!(observed.chain_list_resource_version.as_deref(), Some("20"));
    assert_eq!(
        observed
            .set
            .as_ref()
            .expect("set observation")
            .resource_version,
        "11"
    );
    assert!(!observed.cleanup_bypass_observed);
    assert!(
        handle.next_request().now_or_never().is_none(),
        "unexpected extra remote read"
    );
}

/// Discards otherwise complete withdrawal evidence when a retained identity or generation changes
/// across the child read, protecting Node replacement and registration/set recreation or edit races.
#[tokio::test]
async fn withdrawal_revalidates_node_set_and_registration_after_child_absence() {
    let cases = [
        // Same-name Node replacement invalidates the supported cleanup path.
        ("Node UID changed", ChangedIdentity::NodeUid),
        // An actual set UID is required even when generation and desired spec happen to match.
        ("set UID changed", ChangedIdentity::SetUid),
        // A selector edited and restored after the child read needs a new post-ack observation.
        ("set generation changed", ChangedIdentity::SetGeneration),
        // Recreated registrations stamp a new UID even when their generation matches.
        ("registration UID changed", ChangedIdentity::RegistrationUid),
        // Restoring the same management spec does not restore the prior generation.
        (
            "registration generation changed",
            ChangedIdentity::RegistrationGeneration,
        ),
    ];
    check_cases_async(cases.map(|(scenario, changed)| Case {
        scenario, input: changed, expect: Yields(true),
    }), |changed| async move {
        // Vary only the selected UID or generation while the initial withdrawal resources otherwise match the request.
        let request = request(ServiceVpcChainIntent::Retiring);
        let (sdk, repo) = fixture();
        add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
        insert_registration(&repo, &request);
        let mut handle = add_reader(&repo, "cluster-a");
        let requests = [request.clone()];

        // Cross one selected identity or generation change while the child snapshot is in flight.
        let (observations, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(sdk.observe_service_vpc_chains(&requests), async {
                acknowledge_snapshot(&mut handle, &request).await;
                let (call, sender) = handle.next_request().await.expect("post-ack child list");
                assert_eq!(call.method(), http::Method::GET);
            assert_eq!(call.uri().path(), format!("/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechains"));
                if matches!(changed, ChangedIdentity::RegistrationUid | ChangedIdentity::RegistrationGeneration) {
                    let mut registrations = repo.service_chains.write().expect("chains");
                    let current = registrations.get_mut(&SdkMock::ns_key(TEST_NAMESPACE, &request.definition.name))
                        .expect("current registration");
                    if matches!(changed, ChangedIdentity::RegistrationUid) {
                        // Model recreation with matching generation but newly stamped management UID.
                        current.metadata.uid = Some("replacement-registration-uid".to_owned());
                        current.spec = serde_json::from_value(registered_dpu_service_chain_spec(
                            &request.definition, "replacement-registration-uid"
                        ).expect("replacement spec")).expect("typed replacement spec");
                    } else {
                        // Model two accepted spec writes that return to the same final desired spec.
                        current.metadata.generation = Some(6);
                    }
                    current.metadata.resource_version = Some("6".to_owned());
                }
                // Supply post-acknowledgement absence before final rereads expose the changed resource.
                sender.send_response(list_response::<ServiceChain>(&[], "20"));
                revalidate_snapshot(&mut handle, &request, Some(changed)).await;
            })
        }).await.expect("revalidation observation must finish");

        // Every crossing discards conclusive evidence while retaining the original caller request context.
        let [observed] = observations.as_slice() else { panic!("one result per request"); };
        assert_eq!(observed.request_context, request.request_context);
        Ok::<_, ()>(matches!(observed.state, ServiceVpcChainState::StaleIdentity))
    }).await;
}

/// Preserves a real transient child-list failure as ReadFailure, then obtains a fresh ordered proof
/// on a later call rather than caching failure or treating the failed response as child absence.
#[tokio::test]
async fn failed_child_reads_remain_errors_and_retry_with_fresh_evidence() {
    // Keep desired configuration and retained Node identity unchanged across both observation calls.
    let request = request(ServiceVpcChainIntent::Retiring);
    let (sdk, repo) = fixture();
    add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
    insert_registration(&repo, &request);
    let mut handle = add_reader(&repo, "cluster-a");
    let requests = [request.clone()];

    // Return an actual Kubernetes 503 for the first post-ack child list.
    let (failed, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(sdk.observe_service_vpc_chains(&requests), async {
            acknowledge_snapshot(&mut handle, &request).await;
            let (call, sender) = handle.next_request().await.expect("post-ack child list");
            assert_eq!(call.method(), http::Method::GET);
            assert_eq!(
                call.uri().path(),
                format!(
                    "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechains"
                )
            );
            sender.send_response(
                Response::builder()
                    .status(503)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "apiVersion": "v1", "kind": "Status", "status": "Failure", "code": 503,
                            "reason": "ServiceUnavailable", "message": "injected transient failure",
                        })
                        .to_string()
                        .into_bytes(),
                    ))
                    .expect("failure response"),
            );
        })
    })
    .await
    .expect("failed observation must finish");

    // Failure retains its source and cannot supply a successful absence snapshot.
    let [failed] = failed.as_slice() else {
        panic!("one result per request");
    };
    assert!(
        matches!(&failed.state, ServiceVpcChainState::ReadFailure(error)
        if matches!(error.as_ref(), DpfError::KubeError(kube::Error::Api(status)) if status.code == 503))
    );
    assert!(failed.chain_list_resource_version.is_none());
    assert_eq!(failed.request_context, request.request_context);

    // Retry through new set acknowledgement and child reads, followed by identity revalidation.
    let (retried, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(sdk.observe_service_vpc_chains(&requests), async {
            acknowledge_snapshot(&mut handle, &request).await;
            let (call, sender) = handle
                .next_request()
                .await
                .expect("new post-ack child list");
            assert_eq!(call.method(), http::Method::GET);
            assert_eq!(
                call.uri().path(),
                format!(
                    "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{TEST_NAMESPACE}/servicechains"
                )
            );
            sender.send_response(list_response::<ServiceChain>(&[], "30"));
            revalidate_snapshot(&mut handle, &request, None).await;
        })
    })
    .await
    .expect("retry observation must finish");

    // Only the later successful read can establish isolation for this unchanged request context.
    let [retried] = retried.as_slice() else {
        panic!("one result per request");
    };
    assert!(
        matches!(retried.state, ServiceVpcChainState::Isolated),
        "{retried:?}"
    );
    assert_eq!(retried.chain_list_resource_version.as_deref(), Some("30"));
    assert_eq!(retried.request_context, request.request_context);
}
