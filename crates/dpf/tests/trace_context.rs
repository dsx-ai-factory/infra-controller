/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Request-level proofs for Kubernetes trace annotations.

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use carbide_dpf::KubeRepository;
use carbide_dpf::crds::dpuserviceinterfaces_generated::DPUServiceInterface;
use carbide_dpf::error::DpfError;
use carbide_dpf::repository::{
    DpfOperatorConfigRepository, DpuDeploymentRepository, DpuDeviceRepository,
    DpuNodeMaintenanceRepository, DpuNodeRepository, DpuServiceInterfaceRepository,
};
use http::{Method, Request, Response, StatusCode};
use kube::Client;
use kube::client::Body;
use opentelemetry::trace::{TraceContextExt, TracerProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use serde_json::{Value, json};
use tower_test::mock;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::prelude::*;

const TRACEPARENT: &str = "tracing.k8s.io/traceparent";
const TRACESTATE: &str = "tracing.k8s.io/tracestate";
const TRACE_ID: &str = "00000000000000000000000000000042";

async fn with_trace<T>(state: Option<&str>, action: impl Future<Output = T>) -> T {
    with_trace_id(TRACE_ID, state, action).await
}

async fn with_trace_id<T>(
    trace_id: &str,
    state: Option<&str>,
    action: impl Future<Output = T>,
) -> T {
    // Buffer workers need this binary's global subscriber. INFO excludes Tokio runtime trace spans
    // so the no-active-span cases actually run without a span.
    static PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();
    PROVIDER.get_or_init(|| {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
        let provider = SdkTracerProvider::builder().build();
        tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::INFO)
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("dpf-trace-test")))
            .init();
        provider
    });
    let Some(state) = state else {
        assert!(tracing::Span::current().is_none());
        return action.await;
    };
    let span = tracing::info_span!("dpf_write");
    let headers = http::HeaderMap::from_iter([
        (
            http::HeaderName::from_static("traceparent"),
            format!("00-{trace_id}-0000000000000055-01")
                .parse()
                .unwrap(),
        ),
        (
            http::HeaderName::from_static("tracestate"),
            state.parse().unwrap(),
        ),
    ]);
    trace_propagation::set_span_parent_from_headers(&span, &headers);
    action.instrument(span).await
}

fn current_traceparent() -> String {
    let context = tracing::Span::current().context();
    let span = context.span();
    let context = span.span_context();
    assert_eq!(context.trace_id().to_string(), TRACE_ID);
    assert_ne!(context.span_id().to_string(), "0000000000000055");
    format!("00-{TRACE_ID}-{}-01", context.span_id())
}

#[tokio::test]
async fn create_and_apply_propagate_trace_without_mutating_input() {
    for (scenario, create, state, annotations) in [
        (
            "create preserves metadata",
            true,
            Some("vendor=value"),
            true,
        ),
        (
            "apply initializes annotations",
            false,
            Some("vendor=value"),
            false,
        ),
        ("apply removes stale state", false, Some(""), true),
        ("create omits empty state", true, Some(""), false),
        ("no active span preserves payload", true, None, true),
        ("no active span adds no metadata", false, None, false),
    ] {
        with_trace(state, async {
            let (service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
            let repository = KubeRepository::with_client(Client::new(service, "default"));
            let mut resource = detached_interface();
            resource.metadata.labels = Some([("owner".into(), "nico".into())].into());
            if annotations {
                resource.metadata.annotations = Some(
                    [
                        ("example.com/keep".into(), "keep".into()),
                        (TRACEPARENT.into(), "old-parent".into()),
                        (TRACESTATE.into(), "old=state".into()),
                    ]
                    .into(),
                );
            }
            let original = serde_json::to_value(&resource).unwrap();
            let mut expected = original.clone();
            if let Some(state) = state {
                expected["metadata"]["annotations"][TRACEPARENT] = current_traceparent().into();
                if state.is_empty() {
                    expected["metadata"]["annotations"]
                        .as_object_mut()
                        .unwrap()
                        .remove(TRACESTATE);
                } else {
                    expected["metadata"]["annotations"][TRACESTATE] = state.into();
                }
            }
            let write = async {
                if create {
                    DpuServiceInterfaceRepository::create(&repository, &resource).await
                } else {
                    DpuServiceInterfaceRepository::apply(&repository, &resource).await
                }
            };
            let inspect = async {
                let (request, response) = handle.next_request().await.unwrap();
                assert_eq!(
                    request.method(),
                    if create { Method::POST } else { Method::PATCH }
                );
                assert_eq!(
                    request.headers()[http::header::CONTENT_TYPE],
                    if create {
                        "application/json"
                    } else {
                        "application/apply-patch+yaml"
                    }
                );
                assert!(
                    request
                        .uri()
                        .path()
                        .contains("/namespaces/test-ns/dpuserviceinterfaces")
                );
                let body = request.into_body().collect_bytes().await.unwrap();
                let actual: Value = serde_yaml::from_slice(&body).unwrap();
                assert_eq!(actual, expected, "{scenario}");
                response.send_response(
                    Response::builder()
                        .header(http::header::CONTENT_TYPE, "application/json")
                        .body(Body::from(expected.to_string().into_bytes()))
                        .unwrap(),
                );
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(write, inspect)
            })
            .await
            .expect(scenario);
            result.unwrap();
            assert_eq!(
                serde_json::to_value(resource).unwrap(),
                original,
                "{scenario}"
            );
        })
        .await;
    }
}

#[derive(Clone, Copy)]
enum MergeTarget {
    Device,
    Node,
    Maintenance,
    Deployment,
    OperatorConfig,
}

#[tokio::test]
async fn merge_patches_propagate_trace_and_preserve_patch_semantics() {
    for (scenario, target, state, patch, inject) in [
        (
            "device metadata preserved",
            MergeTarget::Device,
            Some("vendor=value"),
            json!({
                "metadata": {"resourceVersion": "17", "labels": {"keep": "label"},
                    "annotations": {"example.com/keep": "yes", "example.com/remove": null}},
                "spec": {"serialNumber": "test-serial"},
            }),
            true,
        ),
        (
            "node initializes metadata",
            MergeTarget::Node,
            Some("vendor=value"),
            json!({"spec": {"nodeRebootMethod": null}}),
            true,
        ),
        (
            "maintenance initializes annotations",
            MergeTarget::Maintenance,
            Some("vendor=value"),
            json!({"metadata": {"labels": {"keep": "yes"}}}),
            true,
        ),
        (
            "deployment clears stale state",
            MergeTarget::Deployment,
            Some(""),
            json!({"metadata": {"annotations": {
                "example.com/keep": "yes", (TRACEPARENT): "old-parent", (TRACESTATE): "old=state",
            }}}),
            true,
        ),
        (
            "operator config propagates",
            MergeTarget::OperatorConfig,
            Some("vendor=value"),
            json!({"spec": {}}),
            true,
        ),
        (
            "no active span",
            MergeTarget::Device,
            None,
            json!({"spec": {"serialNumber": "test-serial"}}),
            false,
        ),
        (
            "explicit annotation deletion",
            MergeTarget::Device,
            Some("vendor=value"),
            json!({"metadata": {"annotations": null}}),
            false,
        ),
        (
            "explicit metadata deletion",
            MergeTarget::Device,
            Some("vendor=value"),
            json!({"metadata": null}),
            false,
        ),
        (
            "invalid annotation shape",
            MergeTarget::Device,
            Some("vendor=value"),
            json!({"metadata": {"annotations": []}}),
            false,
        ),
        (
            "invalid root shape",
            MergeTarget::Device,
            Some("vendor=value"),
            json!([]),
            false,
        ),
    ] {
        with_trace(state, async {
            let (service, mut handle) = mock::pair::<Request<Body>, Response<Body>>();
            let repository = KubeRepository::with_client(Client::new(service, "default"));
            let mut expected = patch.clone();
            if inject {
                expected["metadata"]["annotations"][TRACEPARENT] = current_traceparent().into();
                expected["metadata"]["annotations"][TRACESTATE] = match state {
                    Some("") => Value::Null,
                    Some(state) => state.into(),
                    None => unreachable!(),
                };
            }
            let write = async {
                match target {
                    MergeTarget::Device => DpuDeviceRepository::patch(&repository, "test", "test-ns", patch).await,
                    MergeTarget::Node => DpuNodeRepository::patch(&repository, "test", "test-ns", patch).await,
                    MergeTarget::Maintenance => DpuNodeMaintenanceRepository::patch(&repository, "test", "test-ns", patch).await,
                    MergeTarget::Deployment => DpuDeploymentRepository::patch(&repository, "test", "test-ns", patch).await,
                    MergeTarget::OperatorConfig => DpfOperatorConfigRepository::patch(&repository, "test", "test-ns", patch).await,
                }
            };
            let inspect = async {
                let (request, response) = handle.next_request().await.unwrap();
                assert_eq!(request.method(), Method::PATCH);
                assert_eq!(request.headers()[http::header::CONTENT_TYPE], "application/merge-patch+json");
                let body = request.into_body().collect_bytes().await.unwrap();
                let actual: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(actual, expected, "{scenario}");
                // The transport error must survive annotation injection unchanged.
                response.send_response(Response::builder().status(StatusCode::CONFLICT)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"status": "Failure", "message": "stale version",
                        "reason": "Conflict", "code": 409}).to_string().into_bytes())).unwrap());
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(write, inspect)
            }).await.expect(scenario);
            assert!(matches!(result, Err(DpfError::KubeError(kube::Error::Api(error))) if error.code == 409), "{scenario}");
        }).await;
    }
}

fn detached_interface() -> DPUServiceInterface {
    serde_json::from_value(json!({
        "apiVersion": "svc.dpu.nvidia.com/v1alpha1",
        "kind": "DPUServiceInterface",
        "metadata": {"name": "service-interface", "namespace": "test-ns"},
        "spec": {
            "template": {
                "metadata": {"labels": {"owner": "id"}},
                "spec": {
                    "nodeSelector": {"matchLabels": {"node": "enabled"}},
                    "template": {
                        "metadata": {"labels": {"owner": "id"}},
                        "spec": {
                            "interfaceType": "patch",
                            "patch": {"peerBridge": "svcbr0"},
                        },
                    },
                },
            },
        },
    }))
    .expect("interface fixture follows the generated CRD schema")
}

// Echo the actual transport payload after both client buffers, including a scheduling boundary.
fn echo_repository() -> KubeRepository {
    let service = tower::service_fn(|request: Request<Body>| async move {
        tokio::task::yield_now().await;
        Ok::<_, kube::Error>(
            Response::builder()
                .header("x-request-uri", request.uri().to_string())
                .header("content-type", request.headers()["content-type"].clone())
                .body(request.into_body())
                .unwrap(),
        )
    });
    KubeRepository::with_client(Client::new(service, "test-ns"))
}

async fn echo_request(
    repository: &KubeRepository,
    method: Method,
    path: &str,
    content_type: &str,
    body: &str,
) -> Vec<u8> {
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        repository.client().send(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", content_type)
                .header("content-length", body.len())
                .body(Body::from(body.as_bytes().to_vec()))
                .unwrap(),
        ),
    )
    .await
    .expect("intercepted request completes")
    .unwrap();
    assert_eq!(response.headers()["x-request-uri"], path);
    assert_eq!(response.headers()["content-type"], content_type);
    response.into_body().collect_bytes().await.unwrap().to_vec()
}

#[tokio::test]
async fn interceptor_covers_supported_write_routes() {
    // Repository tests above exercise serialization. These routes protect interception across
    // repository writes, other API groups, cluster-scoped resources, and subresources.
    let dpf_routes = [
        ("provisioning", "bfbs", "application/json"),
        ("provisioning", "bluefieldsoftwares", "application/json"),
        ("provisioning", "dpudevices", "application/json"),
        ("provisioning", "dpunodes", "application/json"),
        ("provisioning", "dpuflavors", "application/json"),
        ("provisioning", "dpuflavortemplates", "application/json"),
        ("svc", "dpuservices", "application/json"),
        ("svc", "dpuservicechains", "application/json"),
        ("svc", "dpuserviceinterfaces", "application/json"),
        ("provisioning", "dpusets", "application/apply-patch+yaml"),
        (
            "provisioning",
            "dpudeployments",
            "application/apply-patch+yaml",
        ),
        ("svc", "dpuservicetemplates", "application/apply-patch+yaml"),
        (
            "svc",
            "dpuserviceconfigurations",
            "application/apply-patch+yaml",
        ),
        ("svc", "dpuservicenads", "application/apply-patch+yaml"),
        (
            "svc",
            "dpuserviceinterfaces",
            "application/apply-patch+yaml",
        ),
        ("provisioning", "dpudevices", "application/merge-patch+json"),
        ("provisioning", "dpunodes", "application/merge-patch+json"),
        (
            "provisioning",
            "dpunodemaintenances",
            "application/merge-patch+json",
        ),
        (
            "provisioning",
            "dpudeployments",
            "application/merge-patch+json",
        ),
        (
            "operator",
            "dpfoperatorconfigs",
            "application/merge-patch+json",
        ),
    ].map(|(group, resource, content_type)| {
        let create = content_type == "application/json";
        let path = format!("/apis/{group}.dpu.nvidia.com/v1alpha1/namespaces/test-ns/{resource}{}?fieldManager=carbide-dpf-sdk", if create { "" } else { "/test" });
        (if create { Method::POST } else { Method::PATCH }, path, content_type)
    });
    let other_routes = [
        (
            Method::POST,
            "/api/v1/namespaces/test-ns/configmaps",
            "application/json",
        ),
        (
            Method::PATCH,
            "/api/v1/namespaces/test-ns/configmaps/test",
            "application/apply-patch+yaml",
        ),
        (
            Method::PATCH,
            "/api/v1/namespaces/test-ns/secrets/test",
            "application/apply-patch+yaml",
        ),
        (
            Method::PUT,
            "/api/v1/namespaces/test-ns/configmaps/test",
            "application/json",
        ),
        (
            Method::PATCH,
            "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpus/test/status",
            "application/merge-patch+json",
        ),
        (
            Method::PATCH,
            "/apis/apps/v1/namespaces/test-ns/deployments/test/scale",
            "application/merge-patch+json",
        ),
        (
            Method::POST,
            "/apis/other.example/v1/namespaces/test-ns/things",
            "application/json",
        ),
        (Method::POST, "/api/v1/namespaces", "application/json"),
        (
            Method::POST,
            "/apis/other.example/v1/things",
            "application/json",
        ),
    ]
    .map(|(method, path, content_type)| (method, path.to_owned(), content_type));
    for (method, path, content_type) in dpf_routes.into_iter().chain(other_routes) {
        with_trace(Some("vendor=value"), async {
            let repository = echo_repository();
            let expected = current_traceparent();
            let body = echo_request(&repository, method, &path, content_type, "{}").await;
            let actual: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                actual["metadata"]["annotations"][TRACEPARENT], expected,
                "{path}"
            );
            assert_eq!(
                actual["metadata"]["annotations"][TRACESTATE], "vendor=value",
                "{path}"
            );
        })
        .await;
    }
}

#[tokio::test]
async fn excluded_requests_remain_byte_identical() {
    with_trace(Some("vendor=value"), async {
        let repository = echo_repository();
        for (scenario, method, path, content_type, body) in [
            (
                "json patch",
                Method::PATCH,
                "/apis/svc.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpuservices/test",
                "application/json-patch+json",
                "[ ]",
            ),
            (
                "watch",
                Method::GET,
                "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpus?watch=true",
                "application/json",
                "",
            ),
            (
                "delete",
                Method::DELETE,
                "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices/test",
                "application/json",
                "{ }",
            ),
            (
                "strategic merge",
                Method::PATCH,
                "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices/test",
                "application/strategic-merge-patch+json",
                "{ }",
            ),
            (
                "metadata deletion",
                Method::PATCH,
                "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices/test",
                "application/merge-patch+json",
                "{ \"metadata\": null }",
            ),
            (
                "annotation deletion",
                Method::PATCH,
                "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices/test",
                "application/merge-patch+json",
                "{ \"metadata\": {\"annotations\": null} }",
            ),
            (
                "non-object JSON",
                Method::POST,
                "/api/v1/namespaces/test-ns/configmaps",
                "application/json",
                "[ ]",
            ),
            (
                "invalid JSON",
                Method::POST,
                "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices",
                "application/json",
                "invalid",
            ),
            (
                "raw YAML",
                Method::PATCH,
                "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices/test",
                "application/apply-patch+yaml",
                "spec: {}\n",
            ),
        ] {
            assert_eq!(
                echo_request(&repository, method, path, content_type, body).await,
                body.as_bytes(),
                "{scenario}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn concurrent_requests_preserve_exact_caller_context() {
    // Initialize tracing before constructing the shared client, without creating a parent span.
    with_trace(None, async {
        let repository = echo_repository();
        let path = "/apis/provisioning.dpu.nvidia.com/v1alpha1/namespaces/test-ns/dpudevices";
        let send = |trace_id: &'static str| {
            let repository = &repository;
            with_trace_id(trace_id, Some("vendor=value"), async move {
                let context = tracing::Span::current().context();
                let span = context.span();
                let span_context = span.span_context();
                assert_eq!(span_context.trace_id().to_string(), trace_id);
                assert_ne!(span_context.span_id().to_string(), "0000000000000055");
                let expected = format!("00-{trace_id}-{}-01", span_context.span_id());
                let bytes =
                    echo_request(repository, Method::POST, path, "application/json", "{}").await;
                let actual: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(actual["metadata"]["annotations"][TRACEPARENT], expected);
            })
        };
        let (_, _, untraced) = tokio::join!(
            send("00000000000000000000000000000061"),
            send("00000000000000000000000000000062"),
            echo_request(&repository, Method::POST, path, "application/json", "{ }")
        );
        assert_eq!(untraced, b"{ }");
    })
    .await;
}

#[tokio::test]
async fn default_constructor_installs_interceptor() {
    // Kubeconfig belongs to this subprocess, so parallel tests never mutate process environment.
    if std::env::var_os("DPF_TRACE_CONSTRUCTOR_CHILD").is_some() {
        with_trace(Some("vendor=value"), async {
            let repository = KubeRepository::new().await.unwrap();
            assert_eq!(repository.client().default_namespace(), "test-ns");
            let expected = current_traceparent();
            let resource = detached_interface();
            let result = DpuServiceInterfaceRepository::create(&repository, &resource)
                .await
                .unwrap();
            assert_eq!(result.metadata.annotations.unwrap()[TRACEPARENT], expected);
            assert!(resource.metadata.annotations.is_none());
        })
        .await;
        return;
    }
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(config.path(), format!(
        "apiVersion: v1\nkind: Config\nclusters:\n- name: test\n  cluster:\n    server: http://{address}\ncontexts:\n- name: test\n  context:\n    cluster: test\n    namespace: test-ns\ncurrent-context: test\nusers: []\n"
    )).unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "default_constructor_installs_interceptor",
            "--nocapture",
        ])
        .env("DPF_TRACE_CONSTRUCTOR_CHILD", "1")
        .env("KUBECONFIG", config.path())
        .env_remove("HTTPS_PROXY")
        .env_remove("HTTP_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("https_proxy")
        .env_remove("http_proxy")
        .env_remove("all_proxy")
        .kill_on_drop(true);
    let serve = async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = tokio::io::BufReader::new(stream);
        let mut length = None;
        loop {
            let mut line = String::new();
            assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0; length.expect("typed client sends a known-length body")];
        stream.read_exact(&mut body).await.unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .get_mut()
            .write_all(response.as_bytes())
            .await
            .unwrap();
        stream.get_mut().write_all(&body).await.unwrap();
    };
    let (output, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(child.output(), serve)
    })
    .await
    .expect("default client request completes");
    let output = output.unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
