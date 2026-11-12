// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use carbide_test_support::Outcome::{Fails, Yields};
use carbide_test_support::{Case, check_cases_async};
use futures::FutureExt as _;
use http::{Request, Response};
use kube::Client;
use kube::client::Body;
use serde_json::{Value, json};
use tower_test::mock;

use super::NODE_IDENTITY_REQUEST_TIMEOUT;
use crate::repository::DpuDeviceRepository;
use crate::sdk::{DpfSdk, NoLabels, dpu_device_cr_name};
use crate::{DPU_MACHINE_ID_LABEL, DpfError, KubeRepository};

const NS: &str = "node-identity-test";
const DEVICE: &str = "01-02-03-04-05-06";
const DPU_ID: &str = "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg";
const OTHER_DPU_ID: &str = "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng";
const DEVICE_PATH: &str = "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/node-identity-test/dpudevices/device-01-02-03-04-05-06";
type ApiHandle = mock::Handle<Request<Body>, Response<Body>>;

/// Connects the real repository to a mock API so tests can inspect identity patch preconditions.
fn fixture() -> (DpfSdk<KubeRepository>, ApiHandle) {
    let (service, handle) = mock::pair();
    (
        DpfSdk {
            repo: Arc::new(KubeRepository::with_client(Client::new(service, NS))),
            namespace: NS.to_owned(),
            labeler: NoLabels,
            operator_version: semver::Version::new(26, 8, 0),
            shared_bmc_password_ready: Arc::new(AtomicBool::new(false)),
            _bmc_refresh_guard: None,
        },
        handle,
    )
}

/// Supplies a legacy device without a declared Node identity while retaining unrelated cluster data.
fn legacy_device() -> Value {
    json!({
        "apiVersion": "provisioning.dpu.nvidia.com/v1alpha1", "kind": "DPUDevice",
        "metadata": {
            "name": dpu_device_cr_name(DEVICE), "namespace": NS,
            "uid": "device-uid", "resourceVersion": "17", "deletionTimestamp": null,
            "labels": {DPU_MACHINE_ID_LABEL: DPU_ID},
        },
        "spec": {
            "serialNumber": "serial",
            "cluster": {
                "nodeLabels": {"unrelated": "preserve"},
                "nodeAnnotations": {"unrelated": "preserve"},
            },
        },
    })
}

/// Answers an expected device request and returns its body to verify exactly what reaches Kubernetes.
async fn exchange(handle: &mut ApiHandle, method: &str, status: u16, response: Value) -> Value {
    // Inspect the actual method, target and body before answering the SDK.
    let (request, sender) = handle
        .next_request()
        .await
        .expect("expected device request");
    assert_eq!(request.method().as_str(), method);
    assert_eq!(request.uri().path(), DEVICE_PATH);
    let bytes = request
        .into_body()
        .collect_bytes()
        .await
        .expect("read request body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("JSON request")
    };
    sender.send_response(
        Response::builder()
            .status(status)
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(response.to_string().into_bytes()))
            .expect("API response"),
    );
    body
}

/// Bounds mock exchanges so a missing or unexpected request fails instead of hanging the test.
async fn together<T>(operation: impl Future<Output = T>, server: impl Future<Output = ()>) -> T {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (result, ()) = tokio::join!(operation, server);
        result
    })
    .await
    .expect("device exchange must finish")
}

/// Backfills a legacy device using UID and resourceVersion while preserving sibling labels and
/// annotations, so identity propagation cannot replace another controller's cluster configuration.
#[tokio::test]
async fn backfill_is_fenced_and_preserves_unrelated_cluster_fields() {
    // Only the missing DPU identity drives backfill; other cluster fields must remain intact.
    let (sdk, mut handle) = fixture();
    let original = legacy_device();
    let mut expected = original.clone();
    expected["spec"]["cluster"]["nodeLabels"][DPU_MACHINE_ID_LABEL] = json!(DPU_ID);
    expected["metadata"]["resourceVersion"] = json!("18");
    let expected_readback = expected.clone();

    // Apply the SDK operation, then perform a fresh repository read to check persisted fields.
    let operation = async {
        sdk.ensure_dpu_device_node_identity(DEVICE, DPU_ID.parse().expect("DPU ID"))
            .await
            .expect("backfill declared identity");
        let persisted = DpuDeviceRepository::get(&*sdk.repo, &dpu_device_cr_name(DEVICE), NS)
            .await
            .expect("read persisted device")
            .expect("device remains");
        assert_eq!(
            serde_json::to_value(persisted).expect("serialize readback")["spec"],
            expected_readback["spec"]
        );
    };
    let server = async {
        exchange(&mut handle, "GET", 200, original.clone()).await;
        let patch = exchange(&mut handle, "PATCH", 200, expected.clone()).await;

        // Require both fences and a single label delta, rather than a replacement cluster spec.
        assert_eq!(
            patch,
            json!({
                "metadata": {"uid": "device-uid", "resourceVersion": "17"},
                "spec": {"cluster": {"nodeLabels": {DPU_MACHINE_ID_LABEL: DPU_ID}}},
            })
        );
        let mut persisted = original;
        json_patch::merge(&mut persisted, &patch);
        persisted["metadata"]["resourceVersion"] = json!("18");
        assert_eq!(persisted, expected);
        exchange(&mut handle, "GET", 200, persisted).await;
    };
    together(operation, server).await;
}

/// Leaves matching identity unchanged and rejects conflicting, deleting or unfenced devices before
/// patching, preventing Node reassignment and writes based on incomplete Kubernetes observations.
#[tokio::test]
async fn identity_and_patch_preconditions_are_checked_before_mutation() {
    let cases = [
        // An already propagated identity needs no patch.
        (
            "matching identity",
            "/spec/cluster/nodeLabels",
            json!({DPU_MACHINE_ID_LABEL: DPU_ID}),
            Yields(()),
        ),
        // A conflicting remote Node declaration must never be reassigned.
        (
            "different Node identity",
            "/spec/cluster/nodeLabels",
            json!({DPU_MACHINE_ID_LABEL: OTHER_DPU_ID}),
            Fails,
        ),
        // Management identity cannot disagree even when the Node declaration is missing.
        (
            "different management identity",
            "/metadata/labels",
            json!({DPU_MACHINE_ID_LABEL: OTHER_DPU_ID}),
            Fails,
        ),
        // A device being deleted cannot establish a supported Node lifecycle.
        (
            "deleting device",
            "/metadata/deletionTimestamp",
            json!("2026-11-06T00:00:00Z"),
            Fails,
        ),
        // A resource version alone does not supply the required device UID precondition.
        ("missing UID", "/metadata/uid", Value::Null, Fails),
        // A UID alone does not protect concurrent changes to this device.
        (
            "missing resource version",
            "/metadata/resourceVersion",
            Value::Null,
            Fails,
        ),
    ];
    check_cases_async(
        cases
            .into_iter()
            .map(|(scenario, path, value, expect)| Case {
                scenario,
                input: (path, value),
                expect,
            }),
        |(path, value)| async move {
            // Change only this case's identity or fencing field; the rest is a complete legacy device.
            let (sdk, mut handle) = fixture();
            let mut observed = legacy_device();
            *observed.pointer_mut(path).expect("fixture field exists") = value;

            // Return the observation and verify the operation finishes without sending a patch.
            let result = together(
                sdk.ensure_dpu_device_node_identity(DEVICE, DPU_ID.parse().expect("DPU ID")),
                async {
                    exchange(&mut handle, "GET", 200, observed).await;
                },
            )
            .await;
            assert!(
                handle.next_request().now_or_never().is_none(),
                "unexpected mutation"
            );
            result.map_err(drop)
        },
    )
    .await;
}

/// Propagates a rejected identity patch as an error so a replacement device cannot count as a
/// successful backfill and must be read again before retry.
#[tokio::test]
async fn rejected_backfill_remains_an_error() {
    // Observe one device, then model Kubernetes rejecting a stale patch after its replacement.
    let (sdk, mut handle) = fixture();
    let operation = sdk.ensure_dpu_device_node_identity(DEVICE, DPU_ID.parse().expect("DPU ID"));
    let server = async {
        exchange(&mut handle, "GET", 200, legacy_device()).await;
        let patch = exchange(
            &mut handle,
            "PATCH",
            409,
            json!({
                "apiVersion": "v1", "kind": "Status", "status": "Failure",
                "reason": "Conflict", "message": "stale device", "code": 409,
            }),
        )
        .await;

        // The request must carry the original identity rather than targeting a replacement.
        assert_eq!(
            patch["metadata"],
            json!({"uid": "device-uid", "resourceVersion": "17"})
        );
    };

    // Preserve the Kubernetes conflict so the caller retries from a fresh observation.
    assert!(matches!(together(operation, server).await,
        Err(DpfError::KubeError(kube::Error::Api(status))) if status.code == 409));
}

/// Returns an error when either identity attempt stalls, keeping unavailable management reads and
/// uncertain patch outcomes from becoming successful identity evidence.
#[tokio::test(start_paused = true)]
async fn stalled_identity_attempts_fail_at_their_deadline() {
    for method in [
        // An unanswered read cannot establish the device's current identity.
        "GET",
        // An unanswered patch cannot establish that the declared identity was accepted.
        "PATCH",
    ] {
        // Keep the response sender alive so only the SDK's deadline completes the attempt.
        let (sdk, mut handle) = fixture();
        let operation = tokio::spawn(async move {
            sdk.ensure_dpu_device_node_identity(DEVICE, DPU_ID.parse().expect("DPU ID"))
                .await
        });
        if method == "PATCH" {
            exchange(&mut handle, "GET", 200, legacy_device()).await;
        }
        let (request, pending_response) = handle.next_request().await.expect("stalled request");
        assert_eq!(request.method().as_str(), method);

        // Cross the production attempt deadline without spending real wall-clock time.
        tokio::time::advance(NODE_IDENTITY_REQUEST_TIMEOUT + Duration::from_millis(1)).await;
        let result = operation.await.expect("SDK task completes");

        // Both phases must report failure while the server response remains unavailable.
        assert!(matches!(result, Err(DpfError::Timeout { .. })), "{method}");
        drop(pending_response);
    }
}

/// Rejects placement updates that mention the DPU identity so generic label merging cannot bypass
/// the stable identity checks used by exclusion selectors.
#[tokio::test]
async fn placement_updates_cannot_change_or_remove_dpu_identity() {
    check_cases_async(
        [
            // A replacement identity must use the checked identity operation.
            Case {
                scenario: "replacement identity",
                input: Some(OTHER_DPU_ID.to_owned()),
                expect: Fails,
            },
            // Removing identity would invalidate the chain selector's Exists requirement.
            Case {
                scenario: "removed identity",
                input: None,
                expect: Fails,
            },
        ],
        |value| async move {
            // Submit only the reserved identity key; no device read or patch should be needed.
            let (sdk, mut handle) = fixture();
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                sdk.merge_dpu_device_node_labels(
                    DEVICE,
                    BTreeMap::from([(DPU_MACHINE_ID_LABEL.to_owned(), value)]),
                ),
            )
            .await
            .expect("identity rejection must not wait for the API");

            // Reject the operation before it can reach the management API.
            assert!(
                handle.next_request().now_or_never().is_none(),
                "unexpected identity mutation"
            );
            result.map_err(drop)
        },
    )
    .await;
}
