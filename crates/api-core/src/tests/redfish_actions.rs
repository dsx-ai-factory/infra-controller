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

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use carbide_authn::middleware::{ExternalUserInfo, Principal};
use carbide_instrument::testing::{CapturedFieldKind, CapturedLog, capture_logs_async};
use carbide_utils::HostPortPair;
use carbide_utils::redfish::redfish_basic_authorization_context;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use model::redfish::ActionRequest;
use rpc::forge::forge_server::Forge;
use rpc::forge::{RedfishActionId, RedfishBrowseRequest, RedfishCreateActionRequest};
use rustls::pki_types::PrivateKeyDer;
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;

use crate::api::Api;
use crate::auth::AuthContext;
use crate::bmc_proxy::{PassthroughClient, test_config_with_generated_pems};
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::{create_managed_host, create_test_env};
use crate::tests::common::postgres::wait_for_blocked_query;

const ACTION_TARGET: &str = "/redfish/v1/Systems/System.Embedded.1/Actions/ComputerSystem.Reset";

fn request_with_username<T>(user: &str, message: T) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    let mut context = AuthContext::default();
    context
        .principals
        .push(Principal::ExternalUser(ExternalUserInfo::new(
            Some("test_org".to_string()),
            "test_group".to_string(),
            Some(user.to_string()),
        )));
    request.extensions_mut().insert(context);
    request
}

async fn create_approved_action(api: &Api, ips: Vec<String>) -> i64 {
    let request_id = api
        .redfish_create_action(request_with_username(
            "user1",
            RedfishCreateActionRequest {
                ips,
                action: "#ComputerSystem.Reset".to_string(),
                target: ACTION_TARGET.to_string(),
                parameters: "{\"ResetType\":\"ForceOff\"}".to_string(),
            },
        ))
        .await
        .expect("create action")
        .into_inner()
        .request_id;
    api.redfish_approve_action(request_with_username(
        "user2",
        RedfishActionId { request_id },
    ))
    .await
    .expect("approve action");
    request_id
}

async fn fetch_action(pool: &PgPool, request_id: i64) -> ActionRequest {
    db::redfish_actions::fetch_request(
        request_id.into(),
        &mut pool.acquire().await.expect("acquire action reader"),
    )
    .await
    .expect("read action")
}

/// Builds a synthetic TLS endpoint for `subject_alt_name` and returns its PEM
/// identity so direct and CoreProxy tests share a trusted transport boundary.
fn synthetic_tls_endpoint(subject_alt_name: &str) -> (TlsAcceptor, String, String) {
    // Generate one identity accepted by both the server and the proxy client's
    // explicit root set.
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec![subject_alt_name.to_string()]).unwrap();
    let cert_pem = cert.pem();
    let key_pem = signing_key.serialize_pem();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
    )
    .unwrap();
    (TlsAcceptor::from(Arc::new(tls)), cert_pem, key_pem)
}

/// Describes the request headers to verify and the response returned by one
/// synthetic browse endpoint.
struct BrowseResponseSpec {
    expected_path_and_query: String,
    expected_authorization: Option<String>,
    expected_forwarded: Option<String>,
    status: http::StatusCode,
    body: String,
    scenario: &'static str,
}

/// Serves one synthetic TLS response and verifies browse preserves its path
/// while applying only the authorization or proxy-routing headers expected.
async fn serve_browse_response(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    spec: BrowseResponseSpec,
) {
    // Accept one redirected browse request over the same TLS shape used by a BMC.
    let (stream, _) = listener.accept().await.expect("receive browse connection");
    let stream = acceptor.accept(stream).await.expect("accept browse TLS");

    // Verify request preservation before returning the case-specific response.
    hyper::server::conn::http1::Builder::new()
        .keep_alive(false)
        .serve_connection(
            TokioIo::new(stream),
            service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                assert_eq!(request.method(), http::Method::GET, "{}", spec.scenario);
                assert_eq!(
                    request.uri().path_and_query().map(|value| value.as_str()),
                    Some(spec.expected_path_and_query.as_str()),
                    "{}",
                    spec.scenario
                );
                assert_eq!(
                    request
                        .headers()
                        .get(http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok()),
                    spec.expected_authorization.as_deref(),
                    "{}",
                    spec.scenario
                );
                assert_eq!(
                    request
                        .headers()
                        .get("forwarded")
                        .and_then(|value| value.to_str().ok()),
                    spec.expected_forwarded.as_deref(),
                    "{}",
                    spec.scenario
                );
                let body = spec.body.clone();
                async move {
                    Ok::<_, Infallible>(
                        hyper::Response::builder()
                            .status(spec.status)
                            .header("x-browse-case", spec.scenario)
                            .body(Body::from(body))
                            .expect("build browse response"),
                    )
                }
            }),
        )
        .await
        .expect("serve browse response");
}

/// Sends valid HTTP error headers followed by a deliberately incomplete body
/// so browse reaches the post-status body-read failure boundary deterministically.
async fn serve_truncated_browse_response(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    expected_path_and_query: String,
    expected_authorization: String,
) {
    // Complete TLS and read the request before sending a response whose
    // declared length cannot be satisfied by the bytes on the connection.
    let (stream, _) = listener.accept().await.expect("receive browse connection");
    let mut stream = acceptor.accept(stream).await.expect("accept browse TLS");
    let mut request = Vec::new();
    loop {
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).await.expect("read browse request");
        assert_ne!(read, 0, "browse request ended before its headers");
        request.extend_from_slice(&chunk[..read]);
        assert!(
            request.len() <= 16 * 1024,
            "browse request headers too large"
        );
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&request);
    assert!(
        request.starts_with(&format!("GET {expected_path_and_query} HTTP/1.1\r\n")),
        "browse request preserves the requested path and query"
    );
    assert!(
        request
            .lines()
            .any(|line| line
                .eq_ignore_ascii_case(&format!("authorization: {expected_authorization}"))),
        "direct browse request carries fixture Basic authorization"
    );

    // The response status is complete and observable, but EOF arrives before
    // Content-Length so reqwest must fail while consuming the body.
    stream
        .write_all(
            b"HTTP/1.1 502 Bad Gateway\r\n\
              Content-Type: application/json\r\n\
              Content-Length: 128\r\n\
              Connection: close\r\n\
              \r\n\
              {\"error\":{\"message\":\"partial",
        )
        .await
        .expect("write truncated browse response");
    stream.shutdown().await.expect("close truncated response");
}

/// Returns the sole canonical browse failure diagnostic after asserting its
/// shared fields, status type, and sanitized message contract.
fn browse_failure_diagnostic<'a>(
    logs: &'a [CapturedLog],
    requested_uri: &str,
    status: http::StatusCode,
    expected_error: &str,
) -> &'a CapturedLog {
    // Select only this handler's canonical external-failure event so fixture
    // lifecycle logs do not weaken the exact-count assertion.
    let diagnostics = logs
        .iter()
        .filter(|log| {
            log.message == "external call failed"
                && log.field("operation") == Some("redfish_browse")
        })
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 1, "one browse failure diagnostic");
    let diagnostic = diagnostics[0];

    // Assert the stable structured contract shared by every browse failure.
    assert_eq!(diagnostic.level, tracing::Level::WARN);
    assert_eq!(diagnostic.field("backend"), Some("redfish"));
    assert_eq!(diagnostic.field("url"), Some(requested_uri));
    let expected_status = status.as_u16().to_string();
    assert_eq!(
        diagnostic.field("http_status"),
        Some(expected_status.as_str())
    );
    assert_eq!(
        diagnostic.field_kind("http_status"),
        Some(CapturedFieldKind::U64)
    );
    assert_eq!(diagnostic.field("error"), Some(expected_error));
    diagnostic
}

/// Verifies the public browse RPC diagnoses and sanitizes readable HTTP
/// failures without changing successful or otherwise safe response bodies.
#[crate::sqlx_test]
async fn redfish_browse_reports_http_failures_and_preserves_successes(pool: PgPool) {
    struct BrowseCase {
        scenario: &'static str,
        status: http::StatusCode,
        body: String,
        expected_body: String,
        expected_error: Option<String>,
    }

    // Seed real inventory and BMC credentials so the public RPC traverses its
    // normal endpoint lookup and direct-auth request path.
    let env = Box::pin(create_test_env(pool)).await;
    let host = Box::pin(create_managed_host(&env)).await;
    let bmc_ip = host
        .host()
        .rpc_machine()
        .await
        .bmc_info
        .expect("host BMC")
        .ip()
        .to_string();
    let requested_uri = format!("https://{bmc_ip}/redfish/v1/Systems/1?expand=one");
    let expected_path_and_query = "/redfish/v1/Systems/1?expand=one".to_string();
    let fixture_password = "notforprod";
    let (basic_authorization, sensitive_values) =
        redfish_basic_authorization_context("root", Some(fixture_password));
    let basic_payload = sensitive_values[1].clone();
    let message_arg = "diagnostic-must-exclude-message-args";
    let not_found_body = serde_json::json!({
        "error": {
            "message": "fallback message",
            "@Message.ExtendedInfo": [{
                "Message": format!(
                    "missing {fixture_password}; {basic_authorization}; basic {basic_payload}"
                ),
                "MessageArgs": [message_arg],
            }],
        },
    })
    .to_string();
    let redacted_not_found_body = serde_json::json!({
        "error": {
            "message": "fallback message",
            "@Message.ExtendedInfo": [{
                "Message": "missing REDACTED; REDACTED; basic REDACTED",
                "MessageArgs": [message_arg],
            }],
        },
    })
    .to_string();
    let unavailable_body =
        r#"{"error":{"message":"synthetic service temporarily unavailable"}}"#.to_string();

    let cases = [
        // A successful browse stays byte-for-byte intact and emits no failure warning.
        BrowseCase {
            scenario: "successful response",
            status: http::StatusCode::OK,
            body: "{\n  \"Name\": \"Synthetic System\"\n}\n".to_string(),
            expected_body: "{\n  \"Name\": \"Synthetic System\"\n}\n".to_string(),
            expected_error: None,
        },
        // A client error proves every reusable Basic form is masked while
        // MessageArgs remain available to the caller but absent from diagnostics.
        BrowseCase {
            scenario: "credential-echoing client error",
            status: http::StatusCode::NOT_FOUND,
            body: not_found_body,
            expected_body: redacted_not_found_body,
            expected_error: Some("missing REDACTED; REDACTED; basic REDACTED".to_string()),
        },
        // A server error proves the actual status is logged while an already
        // safe response body and its extracted message remain unchanged.
        BrowseCase {
            scenario: "unmatched server error",
            status: http::StatusCode::SERVICE_UNAVAILABLE,
            body: unavailable_body.clone(),
            expected_body: unavailable_body,
            expected_error: Some("synthetic service temporarily unavailable".to_string()),
        },
    ];

    // Build one reusable test certificate; each case gets a fresh listener so
    // the redirected authority is distinct from the original BMC URI.
    let (acceptor, _, _) = synthetic_tls_endpoint("localhost");

    for case in cases {
        // Redirect direct BMC traffic to a synthetic TLS peer without changing
        // the URI that the RPC and diagnostic identify.
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind fake BMC");
        let redirect_address = listener.local_addr().expect("fake BMC address");
        env.api
            .dynamic_settings
            .bmc_proxy
            .store(Arc::new(Some(HostPortPair::HostAndPort(
                redirect_address.ip().to_string(),
                redirect_address.port(),
            ))));

        // Bound both sides independently so an early RPC failure cannot leave
        // the synthetic server waiting indefinitely for a connection.
        let server = tokio::spawn(serve_browse_response(
            listener,
            acceptor.clone(),
            BrowseResponseSpec {
                expected_path_and_query: expected_path_and_query.clone(),
                expected_authorization: Some(basic_authorization.clone()),
                expected_forwarded: Some(format!("host={bmc_ip}")),
                status: case.status,
                body: case.body,
                scenario: case.scenario,
            },
        ));
        let (response, logs) = timeout(
            Duration::from_secs(5),
            capture_logs_async(
                env.api
                    .redfish_browse(tonic::Request::new(RedfishBrowseRequest {
                        uri: requested_uri.clone(),
                    })),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("{}: browse RPC timed out", case.scenario));
        let response = response
            .unwrap_or_else(|error| panic!("{}: browse succeeds: {error}", case.scenario))
            .into_inner();
        timeout(Duration::from_secs(5), server)
            .await
            .unwrap_or_else(|_| panic!("{}: mock BMC timed out", case.scenario))
            .unwrap_or_else(|error| panic!("{}: mock BMC task joins: {error}", case.scenario));

        // Every readable status preserves the browse response and mock header.
        assert_eq!(response.text, case.expected_body, "{}", case.scenario);
        assert_eq!(
            response.headers.get("x-browse-case").map(String::as_str),
            Some(case.scenario),
            "{}",
            case.scenario
        );
        // Success stays silent; failures expose only the canonical safe fields.
        match case.expected_error {
            None => {
                let diagnostics = logs
                    .iter()
                    .filter(|log| {
                        log.message == "external call failed"
                            && log.field("operation") == Some("redfish_browse")
                    })
                    .collect::<Vec<_>>();
                assert!(
                    diagnostics.is_empty(),
                    "{}: successful browse must not warn",
                    case.scenario
                );
            }
            Some(expected_error) => {
                let redirect_uri = format!("https://{redirect_address}");
                let diagnostic =
                    browse_failure_diagnostic(&logs, &requested_uri, case.status, &expected_error);
                assert_ne!(
                    diagnostic.field("url"),
                    Some(redirect_uri.as_str()),
                    "{}: diagnostic must not identify the redirect authority",
                    case.scenario
                );
                assert!(
                    !diagnostic
                        .field("error")
                        .expect("diagnostic error")
                        .contains(message_arg),
                    "{}: MessageArgs must not enter the diagnostic",
                    case.scenario
                );
            }
        }
    }
}

/// Verifies an HTTP error whose body stream terminates early still reports its
/// known status safely before preserving browse's existing RPC read failure.
#[crate::sqlx_test]
async fn redfish_browse_body_read_failure_reports_status_and_preserves_rpc_error(pool: PgPool) {
    // Seed the real direct-auth lookup path and redirect only its network
    // connection to a TLS peer that can truncate the response body.
    let env = Box::pin(create_test_env(pool)).await;
    let host = Box::pin(create_managed_host(&env)).await;
    let bmc_ip = host
        .host()
        .rpc_machine()
        .await
        .bmc_info
        .expect("host BMC")
        .ip()
        .to_string();
    let requested_uri = format!("https://{bmc_ip}/redfish/v1/Systems/1?expand=truncated");
    let expected_path_and_query = "/redfish/v1/Systems/1?expand=truncated".to_string();
    let (basic_authorization, _) = redfish_basic_authorization_context("root", Some("notforprod"));
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind truncated fake BMC");
    let redirect_address = listener.local_addr().expect("truncated fake BMC address");
    env.api
        .dynamic_settings
        .bmc_proxy
        .store(Arc::new(Some(HostPortPair::HostAndPort(
            redirect_address.ip().to_string(),
            redirect_address.port(),
        ))));
    let (acceptor, _, _) = synthetic_tls_endpoint("localhost");

    // Send complete error headers and an incomplete body while capturing the
    // public RPC result and its structured diagnostic.
    let server = tokio::spawn(serve_truncated_browse_response(
        listener,
        acceptor,
        expected_path_and_query,
        basic_authorization,
    ));
    let (result, logs) = timeout(
        Duration::from_secs(5),
        capture_logs_async(
            env.api
                .redfish_browse(tonic::Request::new(RedfishBrowseRequest {
                    uri: requested_uri.clone(),
                })),
        ),
    )
    .await
    .expect("truncated browse RPC finishes");
    timeout(Duration::from_secs(5), server)
        .await
        .expect("truncated mock BMC finishes")
        .expect("truncated mock BMC task joins");

    // Body failure remains an Internal RPC error, while the diagnostic retains
    // the known status and uses the shared unrecognized-response fallback.
    let error = result.expect_err("truncated body must fail the browse RPC");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(error.message().contains("error reading response body"));
    assert!(error.message().contains("status: 502 Bad Gateway"));
    browse_failure_diagnostic(
        &logs,
        &requested_uri,
        http::StatusCode::BAD_GATEWAY,
        "<unrecognized Redfish error response>",
    );
}

/// Verifies the public browse RPC routes through CoreProxy without reading BMC
/// credentials or adding Basic auth, while retaining canonical diagnostics.
#[crate::sqlx_test]
async fn redfish_browse_core_proxy_preserves_credential_ownership_and_diagnostics(pool: PgPool) {
    // Seed inventory in one fixture environment, then build the browse API
    // with its default empty credential manager so any direct lookup fails.
    let env = Box::pin(create_test_env(pool)).await;
    let host = Box::pin(create_managed_host(&env)).await;
    let bmc_ip = host
        .host()
        .rpc_machine()
        .await
        .bmc_info
        .expect("host BMC")
        .ip()
        .to_string();
    let requested_uri = format!("https://{bmc_ip}/redfish/v1/Managers/1?expand=proxy");
    let expected_path_and_query = "/redfish/v1/Managers/1?expand=proxy".to_string();
    let response_body =
        r#"{"error":{"message":"synthetic proxy upstream unavailable"}}"#.to_string();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind fake CoreProxy");
    let proxy_address = listener.local_addr().expect("fake CoreProxy address");
    let (acceptor, cert_pem, key_pem) = synthetic_tls_endpoint("127.0.0.1");

    // Point the production proxy client at the synthetic TLS server and trust
    // only its generated identity; no real proxy or credential store is used.
    let dir = tempfile::tempdir().expect("proxy PEM directory");
    let mut proxy_config = test_config_with_generated_pems(&dir);
    proxy_config.address = proxy_address.to_string();
    std::fs::write(&proxy_config.client_cert, &cert_pem).expect("write proxy client certificate");
    std::fs::write(&proxy_config.client_key, &key_pem).expect("write proxy client key");
    std::fs::write(&proxy_config.root_ca, &cert_pem).expect("write proxy root certificate");
    let mut proxy_api = TestApiBuilder::new(
        env.pool.clone(),
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_runtime_config(env.config.clone())
    .build();
    proxy_api.bmc_proxy_passthrough = Some(Arc::new(
        PassthroughClient::new(&proxy_config).expect("build synthetic proxy client"),
    ));

    // Exercise the public RPC while the server proves proxy routing headers
    // are present and direct Basic authorization is absent.
    let server = tokio::spawn(serve_browse_response(
        listener,
        acceptor,
        BrowseResponseSpec {
            expected_path_and_query,
            expected_authorization: None,
            expected_forwarded: Some(format!("host={bmc_ip}")),
            status: http::StatusCode::BAD_GATEWAY,
            body: response_body.clone(),
            scenario: "CoreProxy HTTP error",
        },
    ));
    let (result, logs) = timeout(
        Duration::from_secs(5),
        capture_logs_async(
            proxy_api.redfish_browse(tonic::Request::new(RedfishBrowseRequest {
                uri: requested_uri.clone(),
            })),
        ),
    )
    .await
    .expect("CoreProxy browse RPC finishes");
    let response = result.expect("readable proxy HTTP error remains a browse response");
    timeout(Duration::from_secs(5), server)
        .await
        .expect("fake CoreProxy finishes")
        .expect("fake CoreProxy task joins");

    // The proxy-owned safe body remains unchanged and Core emits the canonical
    // diagnostic against the original BMC URI rather than proxy authority.
    assert_eq!(response.get_ref().text, response_body);
    assert_eq!(
        response
            .get_ref()
            .headers
            .get("x-browse-case")
            .map(String::as_str),
        Some("CoreProxy HTTP error")
    );
    browse_failure_diagnostic(
        &logs,
        &requested_uri,
        http::StatusCode::BAD_GATEWAY,
        "synthetic proxy upstream unavailable",
    );
}

#[crate::sqlx_test]
async fn failed_claim_commit_does_not_dispatch_and_same_action_can_retry(pool: PgPool) {
    let env = Box::pin(create_test_env(pool.clone())).await;
    let host = Box::pin(create_managed_host(&env)).await;
    let bmc_ip = host
        .host()
        .rpc_machine()
        .await
        .bmc_info
        .expect("host BMC")
        .ip()
        .to_string();
    let request_id = create_approved_action(&env.api, vec![bmc_ip]).await;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind fake BMC");
    let address = listener.local_addr().unwrap();
    env.api
        .dynamic_settings
        .bmc_proxy
        .store(Arc::new(Some(HostPortPair::HostAndPort(
            address.ip().to_string(),
            address.port(),
        ))));

    // The claim UPDATE succeeds; only COMMIT checks this foreign key.
    // This table and constraint live in the test's isolated database.
    sqlx::raw_sql(
        "CREATE TABLE allowed_action_appliers (name text PRIMARY KEY);
         ALTER TABLE redfish_bmc_actions ADD CONSTRAINT test_applier_commit_failure
         FOREIGN KEY (applier) REFERENCES allowed_action_appliers (name)
         DEFERRABLE INITIALLY DEFERRED;",
    )
    .execute(&pool)
    .await
    .expect("install commit-only fault");

    let error = env
        .api
        .redfish_apply_action(request_with_username(
            "user1",
            RedfishActionId { request_id },
        ))
        .await
        .expect_err("claim commit must fail");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(
        error.message().contains("test_applier_commit_failure"),
        "{error}"
    );
    let action = fetch_action(&pool, request_id).await;
    assert!(action.applied_at.is_none());
    assert!(action.applier.is_none());
    assert_eq!(action.results.len(), 1);
    assert!(action.results.iter().all(Option::is_none));
    // Watch for even a TCP connection, not just a completed POST. This is a
    // bounded negative observation because dispatch tasks have no join handle.
    assert!(
        timeout(Duration::from_secs(1), listener.accept())
            .await
            .is_err(),
        "failed claim commit dispatched a request"
    );

    sqlx::query("INSERT INTO allowed_action_appliers (name) VALUES ('user1')")
        .execute(&pool)
        .await
        .expect("allow the same action's retry to commit");
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
    )
    .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let pool = &pool;

    timeout(Duration::from_secs(5), async {
        let (apply, ()) = tokio::join!(
            env.api.redfish_apply_action(request_with_username(
                "user1",
                RedfishActionId { request_id },
            )),
            async {
                let (stream, _) = listener.accept().await.expect("receive action connection");
                let stream = acceptor.accept(stream).await.expect("accept action TLS");
                hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(
                            |request: hyper::Request<hyper::body::Incoming>| async move {
                                assert_eq!(request.method(), http::Method::POST);
                                assert_eq!(request.uri().path(), ACTION_TARGET);
                                let body =
                                    axum::body::to_bytes(Body::new(request.into_body()), 1024)
                                        .await
                                        .expect("read action parameters");
                                assert_eq!(body.as_ref(), b"{\"ResetType\":\"ForceOff\"}");
                                let action = fetch_action(pool, request_id).await;
                                assert!(action.applied_at.is_some(), "claim must precede the POST");
                                assert_eq!(action.applier.as_deref(), Some("user1"));
                                Ok::<_, Infallible>(hyper::Response::new(Body::from(
                                    "action completed",
                                )))
                            },
                        ),
                    )
                    .await
                    .expect("serve action response");
            },
        );
        apply.expect("same action dispatches after its claim commits");
        loop {
            let action = fetch_action(pool, request_id).await;
            if let Some(result) = &action.results[0] {
                assert_eq!(result.status, "200 OK");
                assert_eq!(result.body, "action completed");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("successful action dispatch and result storage finish");
}

#[crate::sqlx_test]
async fn cancellation_after_read_is_not_reported_as_prior_approval_or_apply(pool: PgPool) {
    struct Case {
        scenario: &'static str,
        approve: bool,
        query_fragment: &'static str,
        expected_message: &'static str,
    }

    let env = Box::pin(create_test_env(pool.clone())).await;
    for case in [
        Case {
            scenario: "approval after cancellation",
            approve: true,
            query_fragment: "SET approvers = array_prepend",
            expected_message: "request no longer exists or user already approved it",
        },
        Case {
            scenario: "apply after cancellation",
            approve: false,
            query_fragment: "SET applied_at = now()",
            expected_message: "request no longer exists or was already applied",
        },
    ] {
        let request_id = create_approved_action(&env.api, Vec::new()).await;
        let mut cancellation = pool.begin().await.expect("begin cancellation");
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(cancellation.as_mut())
            .await
            .unwrap();
        db::redfish_actions::delete_request(request_id.into(), cancellation.as_mut())
            .await
            .expect("delete unclaimed action");

        let api = env.api.clone();
        let handler = tokio::spawn(async move {
            let request = request_with_username("user3", RedfishActionId { request_id });
            if case.approve {
                api.redfish_approve_action(request).await.map(|_| ())
            } else {
                api.redfish_apply_action(request).await.map(|_| ())
            }
        });

        // The uncommitted deletion is invisible to the initial SELECT. Wait
        // for the conditional UPDATE before making cancellation visible.
        wait_for_blocked_query(&pool, blocker_pid, case.query_fragment).await;
        cancellation.commit().await.expect("commit cancellation");
        let error = timeout(Duration::from_secs(5), handler)
            .await
            .expect("handler finishes after cancellation")
            .expect("handler task joins")
            .expect_err("cancelled action cannot be approved or applied");
        assert_eq!(
            error.code(),
            tonic::Code::InvalidArgument,
            "{}",
            case.scenario
        );
        assert_eq!(error.message(), case.expected_message, "{}", case.scenario);
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM redfish_bmc_actions WHERE request_id = $1")
                .bind(request_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0, "{}", case.scenario);
    }
}
