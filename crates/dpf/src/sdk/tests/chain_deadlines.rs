// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use futures::FutureExt as _;
use http::{Request, Response};
use kube::Client;
use kube::client::Body;
use serde_json::json;
use tower_test::mock;

use super::registration_chain_definition;
use crate::sdk::{DpfSdk, NoLabels, detached_dpu_service_chain_to_resource};
use crate::{DpfError, KubeRepository};

/// Returns a stage-specific timeout after ten seconds without retrying a stalled chain POST,
/// UID-stamp PATCH or validation-verification GET, so management unavailability cannot trap
/// registration reconciliation.
#[tokio::test(start_paused = true)]
async fn stalled_chain_requests_fail_at_their_deadline_without_retrying() {
    for (methods, expected_operation) in [
        // An unanswered create cannot establish the assigned UID needed for template stamps.
        (&["POST"][..], "create DPUServiceChain"),
        // A successful create must still bound its separate UID-stamp request.
        (&["POST", "PATCH"][..], "patch DPUServiceChain"),
        // A rejected stamp cannot wait indefinitely to distinguish stale identity from validation.
        (
            &["POST", "PATCH", "GET"][..],
            "verify DPUServiceChain patch identity",
        ),
    ] {
        // Vary the stalled request; the definition and API-assigned metadata permit earlier stages.
        let (service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
        let definition = registration_chain_definition();
        let sdk = DpfSdk {
            repo: Arc::new(KubeRepository::with_client(Client::new(
                service,
                &definition.namespace,
            ))),
            namespace: definition.namespace.clone(),
            labeler: NoLabels,
            operator_version: semver::Version::new(26, 10, 0),
            shared_bmc_password_ready: Arc::new(AtomicBool::new(false)),
            _bmc_refresh_guard: None,
        };
        let mut created_chain =
            detached_dpu_service_chain_to_resource(&definition).expect("valid chain");
        created_chain.metadata.uid = Some("registration-uid".to_owned()); // Enables UID stamping.
        created_chain.metadata.resource_version = Some("1".to_owned()); // Fences the first stamp.
        let created_chain = serde_json::to_value(created_chain).expect("created chain response");
        let collection_path = format!(
            "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/{}/dpuservicechains",
            definition.namespace
        );
        let chain_path = format!("{collection_path}/{}", definition.name);

        // Bound the harness at eleven seconds so removing an SDK deadline fails instead of hanging.
        let attempt_started = tokio::time::Instant::now();
        let operation = tokio::time::timeout(
            Duration::from_secs(11),
            sdk.create_detached_dpu_service_chain(&definition),
        );
        tokio::pin!(operation);
        let server = async {
            for (index, method) in methods.iter().enumerate() {
                let (request, sender) = handle.next_request().await.expect("chain request");
                assert_eq!(request.method().as_str(), *method);
                assert_eq!(
                    request.uri().path(),
                    if *method == "POST" {
                        collection_path.as_str()
                    } else {
                        chain_path.as_str()
                    }
                );
                // Hold the selected response until the SDK completes to exclude transport failures.
                if index + 1 == methods.len() {
                    return sender;
                }
                let (status, body) = if *method == "POST" {
                    (201, created_chain.clone())
                } else {
                    (422, json!({"code": 422, "message": "validation rejected"}))
                };
                sender.send_response(
                    Response::builder()
                        .status(status)
                        .header(http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string().into_bytes()))
                        .expect("prerequisite response"),
                );
            }
            unreachable!("each scenario has a stalled request")
        };
        let pending_response = tokio::select! {
            sender = server => sender,
            result = &mut operation => panic!("SDK completed before the selected request: {result:?}"),
        };

        // Remain pending before the deadline, then require the SDK timeout at ten seconds total.
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(operation.as_mut().now_or_never().is_none());
        tokio::time::advance(Duration::from_secs(1)).await;
        let result = operation
            .await
            .expect("SDK deadline must precede the harness deadline");
        assert!(matches!(result,
            Err(DpfError::Timeout { operation, .. }) if operation == expected_operation));
        assert_eq!(attempt_started.elapsed(), Duration::from_secs(10));

        // Completed attempts must neither replay the write nor issue another verification request.
        tokio::task::yield_now().await;
        assert!(
            handle.next_request().now_or_never().is_none(),
            "unexpected retry"
        );
        drop(pending_response);
    }
}
