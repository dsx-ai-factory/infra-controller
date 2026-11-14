// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

pub(in crate::sdk) mod observations;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use async_trait::async_trait;
use carbide_test_support::Outcome::Fails;
use carbide_test_support::{Case, check_cases_async};
use carbide_uuid::machine::DpuMachineId;
use futures::FutureExt as _;
use http::{Request, Response};
use kube::Client;
use kube::client::Body;
use serde_json::{Value, json};
use tower_test::mock;

use super::{SdkMock, TEST_NAMESPACE};
use crate::crds::dpuclusters_generated::DPUCluster;
use crate::crds::dpus_generated::DPU;
use crate::repository::{
    DpuClusterRepository, ServiceVpcClusterReader, ServiceVpcClusterRepository,
};
use crate::sdk::{DpfSdk, NoLabels};
use crate::{DPU_MACHINE_ID_LABEL, DpfError, DpuClusterIdentity};

type ApiHandle = mock::Handle<Request<Body>, Response<Body>>;
const FIRST_DPU: &str = "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg";
const SECOND_DPU: &str = "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng";
const THIRD_DPU: &str = "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80";

#[async_trait]
impl DpuClusterRepository for SdkMock {
    /// Reads the exact namespaced cluster so lookup tests cannot substitute another cluster.
    async fn get(&self, name: &str, namespace: &str) -> Result<Option<DPUCluster>, DpfError> {
        Ok(self
            .clusters
            .read()
            .expect("clusters")
            .get(&Self::ns_key(namespace, name))
            .cloned())
    }

    /// Retains namespace scoping when the SDK mock is used for cluster inventory reads.
    async fn list(&self, namespace: &str) -> Result<Vec<DPUCluster>, DpfError> {
        Ok(self
            .clusters
            .read()
            .expect("clusters")
            .values()
            .filter(|cluster| cluster.metadata.namespace.as_deref() == Some(namespace))
            .cloned()
            .collect())
    }
}

#[async_trait]
impl ServiceVpcClusterRepository for SdkMock {
    /// Records the requested cluster incarnation so SDK routing tests can verify its current UID.
    async fn service_vpc_cluster_reader(
        &self,
        cluster: &DpuClusterIdentity,
    ) -> Result<ServiceVpcClusterReader, DpfError> {
        let current_uid = self
            .clusters
            .read()
            .expect("clusters")
            .get(&Self::ns_key(&cluster.namespace, &cluster.name))
            .and_then(|current| current.metadata.uid.clone());
        if current_uid.as_deref() != Some(cluster.uid.as_str()) {
            return Err(DpfError::InvalidState(
                "observer cluster UID is stale".to_owned(),
            ));
        }
        self.observer_requests
            .write()
            .expect("observer requests")
            .push(cluster.clone());
        let client = self
            .observer_clients
            .read()
            .expect("observer clients")
            .get(&Self::ns_key(&cluster.namespace, &cluster.name))
            .cloned()
            .ok_or_else(|| DpfError::InvalidState("observer client is unavailable".to_owned()))?;
        Ok(ServiceVpcClusterReader::with_client(client))
    }
}

/// Builds an SDK without startup I/O so tests exercise only Node-resolution and chain-observation requests.
fn fixture() -> (DpfSdk<SdkMock>, SdkMock) {
    let repo = SdkMock::new();
    (
        DpfSdk {
            repo: Arc::new(repo.clone()),
            namespace: TEST_NAMESPACE.to_owned(),
            labeler: NoLabels,
            operator_version: semver::Version::new(26, 10, 0),
            shared_bmc_password_ready: Arc::new(AtomicBool::new(false)),
            _bmc_refresh_guard: None,
        },
        repo,
    )
}

/// Supplies one management DPU and cluster so remote Node identity has an authoritative route.
fn add_dpu(repo: &SdkMock, raw_device_id: &str, dpu_id: &str, cluster_name: &str) {
    let dpu: DPU = serde_json::from_value(json!({
        "apiVersion": "provisioning.dpu.nvidia.com/v1alpha1", "kind": "DPU",
        "metadata": {
            "name": format!("{raw_device_id}-node"), "namespace": TEST_NAMESPACE,
            "uid": format!("{raw_device_id}-dpu-uid"), "generation": 1,
            "labels": {DPU_MACHINE_ID_LABEL: dpu_id},
        },
        "spec": {
            "dpuDeviceName": format!("device-{raw_device_id}"), "dpuNodeName": "host",
            "dpuFlavor": "flavor", "serialNumber": format!("{raw_device_id}-serial"),
            "nodeEffect": {"noEffect": true},
            "cluster": {"name": cluster_name, "namespace": TEST_NAMESPACE},
        },
    }))
    .expect("DPU fixture");
    let cluster: DPUCluster = serde_json::from_value(json!({
        "metadata": {"name": cluster_name, "namespace": TEST_NAMESPACE, "uid": format!("{cluster_name}-uid")},
        "spec": {"type": "static"},
    })).expect("cluster fixture");
    repo.dpus
        .write()
        .expect("DPUs")
        .insert(SdkMock::key(&dpu), dpu);
    repo.clusters
        .write()
        .expect("clusters")
        .insert(SdkMock::key(&cluster), cluster);
}

/// Connects one actual cluster route to an API mock so batching cannot silently use another client.
fn add_reader(repo: &SdkMock, cluster_name: &str) -> ApiHandle {
    let (service, handle) = mock::pair();
    repo.observer_clients
        .write()
        .expect("observer clients")
        .insert(
            SdkMock::ns_key(TEST_NAMESPACE, cluster_name),
            Client::new(service, TEST_NAMESPACE),
        );
    handle
}

/// Supplies a remote Node whose name, machine identity and UID can be varied independently.
fn node(raw_device_id: &str, dpu_id: &str) -> Value {
    json!({"metadata": {
        "name": format!("{raw_device_id}-node"), "uid": format!("{raw_device_id}-node-uid"),
        "labels": {DPU_MACHINE_ID_LABEL: dpu_id},
    }})
}

/// Returns one authoritative Node list through the real reader while checking its cluster-scoped path.
async fn respond_nodes(handle: &mut ApiHandle, nodes: Vec<Value>) {
    let (request, sender) = handle.next_request().await.expect("Node list request");
    assert_eq!(request.method(), http::Method::GET);
    assert_eq!(request.uri().path(), "/api/v1/nodes");
    sender.send_response(node_list_response(nodes));
}

/// Supplies complete Node-list metadata so ordering tests fail only on their intended identity change.
fn node_list_response(nodes: Vec<Value>) -> Response<Body> {
    Response::builder().status(200)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({
            "apiVersion": "v1", "kind": "NodeList", "metadata": {"resourceVersion": "7"}, "items": nodes,
        }).to_string().into_bytes())).expect("Node response")
}

/// Resolves several DPUs with one Node list per actual cluster, preventing management-cluster or
/// first-cluster reuse from attaching a DPU to the wrong remote Node.
#[tokio::test]
async fn node_resolution_batches_by_actual_cluster_and_retains_uids() {
    // Put two DPUs in one cluster and a third in another to exercise both batching and routing.
    let (sdk, repo) = fixture();
    add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
    add_dpu(&repo, "second", SECOND_DPU, "cluster-a");
    add_dpu(&repo, "third", THIRD_DPU, "cluster-b");
    let mut first_cluster = add_reader(&repo, "cluster-a");
    let mut second_cluster = add_reader(&repo, "cluster-b");
    let devices = [
        (FIRST_DPU, "first"),
        (SECOND_DPU, "second"),
        (THIRD_DPU, "third"),
    ]
    .map(|(id, device)| {
        (
            id.parse::<DpuMachineId>().expect("DPU ID"),
            device.to_owned(),
        )
    })
    .into_iter()
    .collect();

    // Serve each cluster once while the SDK resolves every requested DPU.
    let (result, (), ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            sdk.resolve_service_vpc_nodes(&devices),
            respond_nodes(
                &mut first_cluster,
                vec![node("first", FIRST_DPU), node("second", SECOND_DPU)]
            ),
            respond_nodes(&mut second_cluster, vec![node("third", THIRD_DPU)]),
        )
    })
    .await
    .expect("Node resolution must finish");
    let resolved = result.expect("current Node identities");

    // Each result must carry its actual cluster UID and Node UID, with no per-DPU duplicate reads.
    assert_eq!(resolved.len(), 3);
    for (id, raw_device_id, cluster_name) in [
        // Siblings in one cluster share the same authoritative list.
        (FIRST_DPU, "first", "cluster-a"),
        // The second sibling must retain its own Node UID.
        (SECOND_DPU, "second", "cluster-a"),
        // A distinct cluster must retain its own management UID and remote Node identity.
        (THIRD_DPU, "third", "cluster-b"),
    ] {
        let identity = &resolved[&id.parse::<DpuMachineId>().expect("DPU ID")];
        assert_eq!(identity.cluster.name, cluster_name);
        assert_eq!(identity.cluster.namespace, TEST_NAMESPACE);
        assert_eq!(identity.cluster.uid, format!("{cluster_name}-uid"));
        assert_eq!(identity.name, format!("{raw_device_id}-node"));
        assert_eq!(identity.uid, format!("{raw_device_id}-node-uid"));
    }
    assert_eq!(
        repo.observer_requests
            .read()
            .expect("observer requests")
            .len(),
        2
    );
    assert!(
        first_cluster.next_request().now_or_never().is_none(),
        "duplicate first-cluster read"
    );
    assert!(
        second_cluster.next_request().now_or_never().is_none(),
        "duplicate second-cluster read"
    );
}

/// Rejects missing, ambiguous, foreign or deleting Node identity so callers cannot retain a Node UID
/// without a unique, live DPU-to-Node mapping.
#[tokio::test]
async fn node_resolution_rejects_incomplete_or_ambiguous_identity() {
    // Keep the management mapping fixed; vary only the remote identity being validated.
    // Remove the propagated label to model a Node cached before identity backfill.
    let mut missing_label = node("first", FIRST_DPU);
    missing_label["metadata"]
        .as_object_mut()
        .expect("metadata")
        .remove("labels");
    // Keep NICo's label while pointing at a different management DPU's Node name.
    let mut foreign_name = node("first", FIRST_DPU);
    foreign_name["metadata"]["name"] = json!("another-dpu-node");
    // Mark the matching Node as deleting before its identity is captured.
    let mut deleting = node("first", FIRST_DPU);
    deleting["metadata"]["deletionTimestamp"] = json!("2026-11-06T00:00:00Z");
    // Drop the immutable UID while retaining the expected name and machine label.
    let mut missing_uid = node("first", FIRST_DPU);
    missing_uid["metadata"]
        .as_object_mut()
        .expect("metadata")
        .remove("uid");
    let cases = [
        // A missing Node must not produce an empty successful baseline.
        ("Node absent", vec![]),
        // A pre-propagation Node cannot yet establish NICo's immutable machine identity.
        ("identity not propagated", vec![missing_label]),
        // Reusing a machine label on two Nodes must remain ambiguous.
        (
            "duplicate identity",
            vec![node("first", FIRST_DPU), node("other", FIRST_DPU)],
        ),
        // Machine identity alone cannot replace the management DPU-to-Node mapping.
        ("foreign Node name", vec![foreign_name]),
        // A finalizing Node cannot be retained as a new activation baseline.
        ("deleting Node", vec![deleting]),
        // A named Node without a UID cannot protect subsequent withdrawal from replacement.
        ("missing Node UID", vec![missing_uid]),
    ];
    check_cases_async(
        cases.map(|(scenario, nodes)| Case {
            scenario,
            input: nodes,
            expect: Fails,
        }),
        |nodes| async move {
            // Provide one current route and one observer for the selected identity failure.
            let (sdk, repo) = fixture();
            add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
            let mut handle = add_reader(&repo, "cluster-a");
            let devices =
                BTreeMap::from([(FIRST_DPU.parse().expect("DPU ID"), "first".to_owned())]);

            // Resolve through the real remote list reader.
            let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(
                    sdk.resolve_service_vpc_nodes(&devices),
                    respond_nodes(&mut handle, nodes)
                )
            })
            .await
            .expect("Node resolution must finish");

            // Every invalid identity must fail the complete lookup, not supply a partial map.
            result.map(drop).map_err(drop)
        },
    )
    .await;
}

/// Rejects ambiguous, unassigned or conflicting management DPUs before contacting a remote observer,
/// preventing initial identity capture from guessing which physical device or cluster to use.
#[tokio::test]
async fn node_resolution_rejects_unusable_management_mappings() {
    enum MappingChange {
        AdditionalDpu,
        NoCluster,
        DifferentDpu,
    }
    let cases = [
        // Deployment replacement can briefly leave two DPU records for one device.
        ("ambiguous DPU incarnation", MappingChange::AdditionalDpu),
        // An initializing DPU has not selected a DPUCluster and cannot use a default one.
        ("cluster not selected", MappingChange::NoCluster),
        // A persisted DPU/device pair must not override a conflicting management machine label.
        (
            "conflicting management identity",
            MappingChange::DifferentDpu,
        ),
    ];
    check_cases_async(
        cases.map(|(scenario, mutation)| Case {
            scenario,
            input: mutation,
            expect: Fails,
        }),
        |mutation| async move {
            // Keep a usable observer available so a guessed route would make an unexpected request.
            let (sdk, repo) = fixture();
            add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
            let _remote = add_reader(&repo, "cluster-a");
            let key = SdkMock::ns_key(TEST_NAMESPACE, "first-node");
            let mut dpu = repo
                .dpus
                .read()
                .expect("DPUs")
                .get(&key)
                .expect("current DPU")
                .clone();
            match mutation {
                MappingChange::AdditionalDpu => {
                    // Add another live incarnation without replacing the original device mapping.
                    dpu.metadata.name = Some("replacement-node".to_owned());
                    dpu.metadata.uid = Some("replacement-dpu-uid".to_owned());
                    repo.dpus
                        .write()
                        .expect("DPUs")
                        .insert(SdkMock::key(&dpu), dpu);
                }
                MappingChange::NoCluster => {
                    // Remove the selected cluster while leaving the device and machine identity intact.
                    dpu.spec.cluster = None;
                    repo.dpus.write().expect("DPUs").insert(key, dpu);
                }
                MappingChange::DifferentDpu => {
                    // Retain a valid route while assigning it to a different persisted DPU identity.
                    dpu.metadata
                        .labels
                        .as_mut()
                        .expect("labels")
                        .insert(DPU_MACHINE_ID_LABEL.to_owned(), SECOND_DPU.to_owned());
                    repo.dpus.write().expect("DPUs").insert(key, dpu);
                }
            }
            let devices =
                BTreeMap::from([(FIRST_DPU.parse().expect("DPU ID"), "first".to_owned())]);

            // Resolve through the public SDK operation with a bounded test wait.
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                sdk.resolve_service_vpc_nodes(&devices),
            )
            .await
            .expect("management validation must finish");

            // Invalid management identity must fail before opening any observer client.
            assert!(
                repo.observer_requests
                    .read()
                    .expect("observer requests")
                    .is_empty()
            );
            result.map(drop).map_err(drop)
        },
    )
    .await;
}

/// Rejects management DPU or DPUCluster replacement during a remote read, preventing an identity
/// obtained through the prior mapping from being retained as the current activation baseline.
#[tokio::test]
async fn node_resolution_revalidates_management_identity_after_remote_reads() {
    let cases = [
        // A new management DPU UID invalidates the physical-device mapping even on the same cluster.
        ("DPU replaced", false),
        // A same-name DPUCluster replacement invalidates routing even when the DPU record is unchanged.
        ("DPUCluster replaced", true),
    ];
    check_cases_async(
        cases.map(|(scenario, replaces_cluster)| Case {
            scenario,
            input: replaces_cluster,
            expect: Fails,
        }),
        |replaces_cluster| async move {
            // Start from one complete DPU route and remote Node identity.
            let (sdk, repo) = fixture();
            add_dpu(&repo, "first", FIRST_DPU, "cluster-a");
            let mut handle = add_reader(&repo, "cluster-a");
            let devices =
                BTreeMap::from([(FIRST_DPU.parse().expect("DPU ID"), "first".to_owned())]);

            // Wait for the remote request before replacing the selected management UID.
            let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(sdk.resolve_service_vpc_nodes(&devices), async {
                    let (request, sender) = handle.next_request().await.expect("Node list request");
                    assert_eq!(request.uri().path(), "/api/v1/nodes");
                    if replaces_cluster {
                        repo.clusters
                            .write()
                            .expect("clusters")
                            .get_mut(&SdkMock::ns_key(TEST_NAMESPACE, "cluster-a"))
                            .expect("current cluster")
                            .metadata
                            .uid = Some("replacement-cluster-uid".to_owned());
                    } else {
                        repo.dpus
                            .write()
                            .expect("DPUs")
                            .get_mut(&SdkMock::ns_key(TEST_NAMESPACE, "first-node"))
                            .expect("current DPU")
                            .metadata
                            .uid = Some("replacement-dpu-uid".to_owned());
                    }
                    sender.send_response(node_list_response(vec![node("first", FIRST_DPU)]));
                })
            })
            .await
            .expect("Node resolution must finish");

            // The management reread must discard identities obtained through the prior incarnation.
            result.map(drop).map_err(drop)
        },
    )
    .await;
}
