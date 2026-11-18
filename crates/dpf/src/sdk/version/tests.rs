/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::time::Duration;

use futures::FutureExt;
use http::{Request, Response};
use kube::Client;
use kube::client::Body;
use serde_json::{Value, json};
use tower_test::mock;

use super::OPERATOR_VERSION_READ_TIMEOUT;
use crate::sdk::{DpfSdkBuilder, SECRET_NAME};
use crate::{DpfError, KubeRepository};

const NS: &str = "startup-version-ns";
type ApiHandle = mock::Handle<Request<Body>, Response<Body>>;

async fn respond(handle: &mut ApiHandle, method: &str, path: &str, status: u16, response: Value) {
    let (request, sender) = handle
        .next_request()
        .await
        .expect("expected Kubernetes request");
    assert_eq!(request.method().as_str(), method);
    assert_eq!(request.uri().path(), path);
    sender.send_response(
        Response::builder()
            .status(status)
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(response.to_string().into_bytes()))
            .unwrap(),
    );
}

fn operator_config(version: Option<&str>) -> Value {
    json!({
        "apiVersion": "operator.dpu.nvidia.com/v1alpha1",
        "kind": "DPFOperatorConfig",
        "metadata": {"name": "dpfoperatorconfig", "namespace": NS},
        "spec": {"provisioningController": {}},
        "status": {"version": version, "targetVersion": "v99.0.0"},
    })
}

#[tokio::test]
async fn startup_caches_installed_version_and_gates_bmc_credentials_without_more_requests() {
    let cases = [
        ("minimum release", "v26.8.0", "26.8.0", true),
        (
            "minimum nightly",
            "v26.8.0-5a1c2302-nightly",
            "26.8.0-5a1c2302-nightly",
            false,
        ),
        (
            "higher patch nightly",
            "v26.8.1-nightly",
            "26.8.1-nightly",
            true,
        ),
    ];
    for (scenario, version, expected_version, supported) in cases {
        let (service, mut handle) = mock::pair();
        let repo = KubeRepository::with_client(Client::new(service, "default"));
        let operation =
            DpfSdkBuilder::new(repo, NS, "shared-password".to_string()).build_without_resources();
        let server = async {
            respond(&mut handle, "GET", &format!("/apis/operator.dpu.nvidia.com/v1alpha1/namespaces/{NS}/dpfoperatorconfigs/dpfoperatorconfig"), 200, operator_config(Some(version))).await;
            respond(&mut handle, "PATCH", &format!("/api/v1/namespaces/{NS}/secrets/{SECRET_NAME}"), 200,
                json!({"apiVersion": "v1", "kind": "Secret", "metadata": {"name": SECRET_NAME, "namespace": NS}})).await;
        };
        let (sdk, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(operation, server)
        })
        .await
        .expect("startup requests must complete");
        let sdk = sdk.unwrap();
        for _ in 0..2 {
            assert_eq!(
                sdk.operator_version().to_string(),
                expected_version,
                "{scenario}"
            );
            assert_eq!(
                sdk.supports_per_dpu_bmc_credentials(),
                supported,
                "{scenario}"
            );
        }
        assert!(
            handle.next_request().now_or_never().is_none(),
            "cached checks must not query Kubernetes: {scenario}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn startup_requires_readable_operator_config_and_valid_version() {
    struct Case {
        name: &'static str,
        response: Option<(u16, Value)>,
        check: fn(&DpfError) -> bool,
    }
    let cases = [
        Case {
            name: "missing operator config",
            response: Some((
                404,
                json!({
                    "apiVersion": "v1", "kind": "Status", "status": "Failure",
                    "code": 404, "reason": "NotFound", "message": "missing operator config",
                }),
            )),
            check: DpfError::is_not_found,
        },
        Case {
            name: "missing version",
            response: Some((200, operator_config(None))),
            check: |error| matches!(error, DpfError::InvalidState(message) if message.contains("no status.version")),
        },
        Case {
            name: "invalid version",
            response: Some((200, operator_config(Some("invalid")))),
            check: |error| matches!(error, DpfError::InvalidState(message) if message.contains("invalid status.version")),
        },
        Case {
            name: "stalled version read",
            response: None,
            check: |error| matches!(error, DpfError::Timeout { .. }),
        },
    ];

    for case in cases {
        let (service, mut handle) = mock::pair();
        let repo = KubeRepository::with_client(Client::new(service, "default"));
        let startup = tokio::spawn(
            DpfSdkBuilder::new(repo, NS, "shared-password".to_string()).build_without_resources(),
        );
        let version_path = format!(
            "/apis/operator.dpu.nvidia.com/v1alpha1/namespaces/{NS}/dpfoperatorconfigs/dpfoperatorconfig"
        );
        let _stalled_response = if let Some((status, response)) = case.response {
            respond(&mut handle, "GET", &version_path, status, response).await;
            None
        } else {
            let (request, sender) = handle.next_request().await.expect("expected version GET");
            assert_eq!(request.uri().path(), version_path);
            tokio::time::advance(OPERATOR_VERSION_READ_TIMEOUT).await;
            Some(sender)
        };
        let error = tokio::time::timeout(Duration::from_secs(1), startup)
            .await
            .expect("SDK initialization must finish after the version read deadline")
            .unwrap()
            .err()
            .expect("SDK startup must fail");
        assert!((case.check)(&error), "{}: {error}", case.name);
    }
}
