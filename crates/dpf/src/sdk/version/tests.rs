/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::time::Duration;

use futures::FutureExt;
use http::{Request, Response};
use kube::Client;
use kube::client::Body;
use semver::Version;
use serde_json::{Value, json};
use tower_test::mock;

use super::{OPERATOR_VERSION_READ_TIMEOUT, VersionRequirement, version_requirement};
use crate::sdk::{DpfSdkBuilder, SECRET_NAME};
use crate::{DpfError, KubeRepository};

const NS: &str = "startup-version-ns";
type ApiHandle = mock::Handle<Request<Body>, Response<Body>>;

/// The required version uses semver precedence, including prereleases and build metadata.
#[test]
fn operator_version_requirement() {
    for (scenario, installed, required, succeeds) in [
        ("older release", "29.0.0", "30.0.0", false),
        ("RC below final", "30.0.0-rc.4", "30.0.0", false),
        ("numeric RC ordering", "30.0.0-rc.10", "30.0.0-rc.4", true),
        ("exact version", "30.0.0", "30.0.0", true),
        ("newer release", "30.0.1", "30.0.0", true),
        ("metadata ignored", "30.0.0+aaa", "30.0.0+zzz", true),
    ] {
        let requirement = VersionRequirement {
            version: Version::parse(required).unwrap(),
        };
        let result = requirement.check_operator_version(&Version::parse(installed).unwrap());
        assert_eq!(result.is_ok(), succeeds, "{scenario}: {result:?}");
    }
}

/// A rejected version cannot write the shared BMC Secret or start resource initialization.
#[tokio::test]
async fn startup_rejects_version_requirements_before_writes() {
    let (service, mut handle) = mock::pair();
    let repo = KubeRepository::with_client(Client::new(service, "default"));
    let operation = DpfSdkBuilder::new(repo, NS, "shared-password".to_string())
        .with_labeler(crate::sdk::NoLabels)
        .build_without_resources();
    let server = async {
        respond(&mut handle, "GET", &format!("/apis/operator.dpu.nvidia.com/v1alpha1/namespaces/{NS}/dpfoperatorconfigs/dpfoperatorconfig"), 200, operator_config(Some("v0.0.0"))).await;
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(operation, server)
    })
    .await
    .expect("startup rejection must finish");
    let error = result
        .err()
        .expect("startup must reject an unsupported operator");
    assert!(
        error.to_string().contains(&format!(
            "installed DPF operator version 0.0.0 is below the minimum {} required by NICo",
            version_requirement().unwrap().version,
        )),
        "{error}"
    );
    assert!(
        !matches!(handle.next_request().now_or_never(), Some(Some(_))),
        "unsupported operator must not cause Kubernetes writes"
    );
}

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
    let version = version_requirement().unwrap().version;
    let expected_version = version.to_string();
    let version = format!("v{version}");
    let (service, mut handle) = mock::pair();
    let repo = KubeRepository::with_client(Client::new(service, "default"));
    let operation =
        DpfSdkBuilder::new(repo, NS, "shared-password".to_string()).build_without_resources();
    let server = async {
        respond(&mut handle, "GET", &format!("/apis/operator.dpu.nvidia.com/v1alpha1/namespaces/{NS}/dpfoperatorconfigs/dpfoperatorconfig"), 200, operator_config(Some(&version))).await;
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
            "{version}"
        );
        assert!(sdk.supports_per_dpu_bmc_credentials(), "{version}");
    }
    assert!(
        handle.next_request().now_or_never().is_none(),
        "cached checks must not query Kubernetes: {version}"
    );
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
