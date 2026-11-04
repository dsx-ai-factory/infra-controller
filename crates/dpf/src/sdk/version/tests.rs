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

use crate::KubeRepository;
use crate::sdk::{DpfSdkBuilder, SECRET_NAME};

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
        (
            "minimum release",
            200,
            operator_config(Some("v26.8.0")),
            Some("26.8.0"),
            true,
        ),
        (
            "minimum nightly",
            200,
            operator_config(Some("v26.8.0-5a1c2302-nightly")),
            Some("26.8.0-5a1c2302-nightly"),
            false,
        ),
        (
            "higher patch nightly",
            200,
            operator_config(Some("v26.8.1-nightly")),
            Some("26.8.1-nightly"),
            true,
        ),
        (
            "invalid version",
            200,
            operator_config(Some("not-a-version")),
            None,
            false,
        ),
        (
            "target version only",
            200,
            operator_config(None),
            None,
            false,
        ),
        (
            "operator read failed",
            503,
            json!({
                "apiVersion": "v1", "kind": "Status", "status": "Failure",
                "code": 503, "reason": "ServiceUnavailable", "message": "unavailable",
            }),
            None,
            false,
        ),
    ];

    for (scenario, code, response, expected_version, supported) in cases {
        let (service, mut handle) = mock::pair();
        let repo = KubeRepository::with_client(Client::new(service, "default"));
        let operation =
            DpfSdkBuilder::new(repo, NS, "shared-password".to_string()).build_without_resources();
        let server = async {
            respond(&mut handle, "PATCH", &format!("/api/v1/namespaces/{NS}/secrets/{SECRET_NAME}"), 200,
                json!({"apiVersion": "v1", "kind": "Secret", "metadata": {"name": SECRET_NAME, "namespace": NS}})).await;
            respond(&mut handle, "GET", &format!("/apis/operator.dpu.nvidia.com/v1alpha1/namespaces/{NS}/dpfoperatorconfigs/dpfoperatorconfig"), code, response).await;
        };
        let (sdk, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(operation, server)
        })
        .await
        .expect("startup requests must complete");
        let sdk = sdk.unwrap();
        for _ in 0..3 {
            assert_eq!(
                sdk.operator_version().map(ToString::to_string).as_deref(),
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
