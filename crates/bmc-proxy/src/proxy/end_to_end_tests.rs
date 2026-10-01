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

//! One proxied request against a TLS fake BMC, from the handler's ACL check
//! to the answer, as the caller and the BMC observe it. Ingress and the
//! allow-list middleware run before the handler and have their own tests.

use std::convert::Infallible;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, Method, Request, StatusCode, header};
use axum::response::IntoResponse;
use bytes::Bytes;
use carbide_authn::middleware::AuthContext;
use carbide_instrument::testing::MetricsCapture;
use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use carbide_utils::redfish::redfish_basic_authorization_context;
use futures_util::StreamExt;
use mac_address::MacAddress;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use rpc::forge;
use rpc::forge::find_bmc_ips_request::LookupBy;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio_rustls::rustls;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tracing_subscriber::layer::SubscriberExt;

use crate::proxy::credentials::BmcCredentials;
use crate::proxy::test_support::*;
use crate::proxy::upstream::MAX_BUFFERED_BODY_SIZE;
use crate::proxy::{BmcProxyState, proxy_request};

/// The fake BMC listens here, so the proxy reaches it at the IP it resolves,
/// with only the port overridden, as it reaches a real BMC.
const FAKE_BMC_IP: &str = "127.0.0.1";
const BMC_PASSWORD: &str = "bmc-secret";
/// What nico-api hands out once the BMC rejects the cached credential.
const FRESH_PASSWORD: &str = "bmc-rotated";
const SYSTEM_PATH: &str = "/redfish/v1/Systems/System_0";
const SYSTEM_BODY: &str = r#"{"PowerState":"On"}"#;
/// Redirects to [`SYSTEM_PATH`].
const MOVED_PATH: &str = "/redfish/v1/Moved";
/// Answers a 500 whose body echoes the BMC password.
const LEAKY_PATH: &str = "/redfish/v1/Leaky";
/// Accepts a body and answers with its length.
const UPLOAD_PATH: &str = "/redfish/v1/UpdateService/upload";
/// Accepts only [`FRESH_PASSWORD`].
const ROTATED_PATH: &str = "/redfish/v1/Rotated";
/// Rejects every credential, echoing the password it was sent.
const REJECTING_PATH: &str = "/redfish/v1/UpdateService/rejecting";
/// Answers after [`SLOW_ANSWER_DELAY`].
const SLOW_PATH: &str = "/redfish/v1/Slow";
/// Answers after two and a half seconds.
const BRIEFLY_SLOW_PATH: &str = "/redfish/v1/BrieflySlow";
/// Rejects the cached credential at once, and answers [`FRESH_PASSWORD`]
/// after two and a half seconds.
const BRIEFLY_SLOW_ROTATED_PATH: &str = "/redfish/v1/BrieflySlowRotated";
/// Rejects the cached credential at once, and answers [`FRESH_PASSWORD`]
/// after [`SLOW_ANSWER_DELAY`].
const SLOW_ROTATED_PATH: &str = "/redfish/v1/SlowRotated";
/// Answers at once with part of its body, and sends the rest after
/// [`SLOW_ANSWER_DELAY`].
const SLOW_BODY_PATH: &str = "/redfish/v1/SlowBody";
/// Ten times [`QUICK_CLASS`]'s budget, and a third of the default budget:
/// a request held to its class's budget is cut off long before the BMC
/// answers, and one held to the default budget gets the answer. A test
/// waits only for the budget, never for this.
const SLOW_ANSWER_DELAY: Duration = Duration::from_secs(20);

fn fake_bmc_mac() -> MacAddress {
    MacAddress::new([0x02, 0, 0, 0, 0, 0x08])
}

fn basic(password: &str) -> String {
    redfish_basic_authorization_context("root", Some(password)).0
}

/// One request as the fake BMC received it.
#[derive(Debug, Clone)]
struct Received {
    method: Method,
    path_and_query: String,
    headers: HeaderMap,
    body_len: usize,
}

/// A TLS listener standing in for one BMC, recording what it received.
#[derive(Clone, Default)]
struct FakeBmc {
    received: Arc<Mutex<Vec<Received>>>,
}

impl FakeBmc {
    fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }
}

async fn fake_bmc_handler(
    axum::extract::State(bmc): axum::extract::State<FakeBmc>,
    request: Request<Body>,
) -> axum::response::Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX)
        .await
        .expect("the request body reads");
    bmc.received.lock().unwrap().push(Received {
        method: parts.method.clone(),
        path_and_query: parts
            .uri
            .path_and_query()
            .map(ToString::to_string)
            .unwrap_or_default(),
        headers: parts.headers.clone(),
        body_len: body.len(),
    });
    let authorization = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    match (parts.method, parts.uri.path()) {
        (Method::GET, SYSTEM_PATH) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (HeaderName::from_static("x-bmc-custom"), "kept"),
            ],
            SYSTEM_BODY,
        )
            .into_response(),
        (Method::GET, MOVED_PATH) => (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, SYSTEM_PATH)],
        )
            .into_response(),
        (Method::GET, LEAKY_PATH) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(r#"{{"error":"rejected password {BMC_PASSWORD}"}}"#),
        )
            .into_response(),
        (Method::POST, UPLOAD_PATH) => {
            (StatusCode::ACCEPTED, body.len().to_string()).into_response()
        }
        (Method::GET, ROTATED_PATH) if authorization == Some(basic(FRESH_PASSWORD).as_str()) => {
            SYSTEM_BODY.into_response()
        }
        (Method::GET, ROTATED_PATH) => StatusCode::UNAUTHORIZED.into_response(),
        (Method::GET, BRIEFLY_SLOW_PATH) => {
            tokio::time::sleep(Duration::from_millis(2500)).await;
            SYSTEM_BODY.into_response()
        }
        (Method::GET, BRIEFLY_SLOW_ROTATED_PATH)
            if authorization == Some(basic(FRESH_PASSWORD).as_str()) =>
        {
            tokio::time::sleep(Duration::from_millis(2500)).await;
            SYSTEM_BODY.into_response()
        }
        (Method::GET, BRIEFLY_SLOW_ROTATED_PATH) => StatusCode::UNAUTHORIZED.into_response(),
        (Method::GET, SLOW_PATH) => {
            tokio::time::sleep(SLOW_ANSWER_DELAY).await;
            SYSTEM_BODY.into_response()
        }
        (Method::GET, SLOW_BODY_PATH) => {
            let parts = futures_util::stream::iter([false, true]).then(|late| async move {
                if late {
                    tokio::time::sleep(SLOW_ANSWER_DELAY).await;
                }
                Ok::<_, Infallible>(Bytes::from_static(b"{}"))
            });
            Body::from_stream(parts).into_response()
        }
        (Method::GET, SLOW_ROTATED_PATH)
            if authorization == Some(basic(FRESH_PASSWORD).as_str()) =>
        {
            tokio::time::sleep(SLOW_ANSWER_DELAY).await;
            SYSTEM_BODY.into_response()
        }
        (Method::GET, SLOW_ROTATED_PATH) => StatusCode::UNAUTHORIZED.into_response(),
        (_, REJECTING_PATH) => {
            let sent = [BMC_PASSWORD, FRESH_PASSWORD]
                .into_iter()
                .find(|password| authorization == Some(basic(password).as_str()))
                .unwrap_or("none");
            (
                StatusCode::UNAUTHORIZED,
                format!(r#"{{"error":"password {sent} rejected"}}"#),
            )
                .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn spawn_fake_bmc() -> (SocketAddr, FakeBmc) {
    let params = rcgen::CertificateParams::new(vec![FAKE_BMC_IP.to_string()]).expect("cert params");
    let key = rcgen::KeyPair::generate().expect("server key");
    let cert = params.self_signed(&key).expect("self-signed server cert");
    let server_cert: CertificateDer<'static> = cert.der().clone();
    let server_key = PrivateKeyDer::Pkcs8(key.serialize_der().into());
    // The test binary has more than one rustls crypto provider compiled in,
    // so name the one the proxy's own listener uses. The proxy's upstream
    // client accepts any certificate, as it does for real BMCs, so a
    // self-signed one suffices.
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS protocol versions")
    .with_no_client_auth()
    .with_single_cert(vec![server_cert], server_key)
    .expect("server TLS config");

    let bmc = FakeBmc::default();
    let app = axum::Router::new()
        .route("/{*path}", axum::routing::any(fake_bmc_handler))
        .with_state(bmc.clone());
    let listener = std::net::TcpListener::bind((FAKE_BMC_IP, 0)).expect("bind fake BMC");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let addr = listener.local_addr().expect("fake BMC address");
    tokio::spawn(async move {
        axum_server::from_tcp_rustls(
            listener,
            axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(config)),
        )
        .expect("fake BMC listener")
        .serve(app.into_make_service())
        .await
        .expect("fake BMC serves");
    });
    (addr, bmc)
}

/// A port on the BMC's IP that refuses connections. The socket stays bound,
/// without listening, for as long as the value lives, so no other test's
/// listener can take the port meanwhile.
fn refusing_port() -> (tokio::net::TcpSocket, u16) {
    let socket = tokio::net::TcpSocket::new_v4().expect("socket");
    socket
        .bind((FAKE_BMC_IP.parse::<IpAddr>().unwrap(), 0).into())
        .expect("bind");
    let port = socket.local_addr().expect("address").port();
    (socket, port)
}

/// nico-api as the proxy uses it to replace a credential: naming the BMC at
/// an IP, and handing out that BMC's credential, [`FRESH_PASSWORD`], each
/// after `lookup_delay`. It also answers `Version`, which the client calls to
/// check its connection, at once.
#[derive(Clone)]
struct FakeNicoApi {
    lookup_delay: Duration,
}

impl tonic::server::NamedService for FakeNicoApi {
    const NAME: &'static str = "forge.Forge";
}

impl tower::Service<http::Request<tonic::body::Body>> for FakeNicoApi {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<tonic::body::Body>) -> Self::Future {
        let lookup_delay = self.lookup_delay;
        Box::pin(async move {
            if request.uri().path() != "/forge.Forge/Version" {
                tokio::time::sleep(lookup_delay).await;
            }
            Ok(match request.uri().path() {
                "/forge.Forge/Version" => {
                    tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
                        .unary(
                            Unary(|_: forge::VersionRequest| forge::BuildInfo::default()),
                            request,
                        )
                        .await
                }
                "/forge.Forge/FindMacAddressByBmcIp" => {
                    tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
                        .unary(
                            Unary(|ip: forge::BmcIp| forge::MacAddressBmcIp {
                                bmc_ip: ip.bmc_ip,
                                mac_address: fake_bmc_mac().to_string(),
                            }),
                            request,
                        )
                        .await
                }
                "/forge.Forge/GetBmcCredentials" => {
                    tonic::server::Grpc::new(tonic_prost::ProstCodec::default())
                        .unary(
                            Unary(|_: forge::GetBmcCredentialsRequest| {
                                forge::GetBmcCredentialsResponse {
                                    credentials: Some(forge::BmcCredentials {
                                        r#type: Some(
                                            forge::bmc_credentials::Type::UsernamePassword(
                                                forge::UsernamePassword {
                                                    username: "root".to_string(),
                                                    password: FRESH_PASSWORD.to_string(),
                                                },
                                            ),
                                        ),
                                    }),
                                }
                            }),
                            request,
                        )
                        .await
                }
                _ => tonic::Status::unimplemented("not faked").into_http(),
            })
        })
    }
}

/// A unary gRPC method answered by `F`.
#[derive(Clone)]
struct Unary<F>(F);

impl<Req, Resp, F> tonic::server::UnaryService<Req> for Unary<F>
where
    F: Fn(Req) -> Resp,
    Req: Send + 'static,
    Resp: Send + 'static,
{
    type Response = Resp;
    type Future = std::future::Ready<Result<tonic::Response<Resp>, tonic::Status>>;

    fn call(&mut self, request: tonic::Request<Req>) -> Self::Future {
        std::future::ready(Ok(tonic::Response::new((self.0)(request.into_inner()))))
    }
}

/// A client for a fake nico-api listening on a fresh local port.
async fn fake_nico_api() -> ForgeApiClient {
    fake_nico_api_answering_after(Duration::ZERO).await
}

/// [`fake_nico_api`], answering lookups after `lookup_delay`.
async fn fake_nico_api_answering_after(lookup_delay: Duration) -> ForgeApiClient {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake nico-api");
    let addr = listener.local_addr().expect("fake nico-api address");
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(FakeNicoApi { lookup_delay })
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("fake nico-api serves");
    });
    ForgeApiClient::new(&ApiConfig::new(
        &format!("http://{addr}"),
        &ForgeClientConfig::default(),
    ))
}

fn root_password() -> BmcCredentials {
    BmcCredentials::UsernamePassword {
        username: "root".to_string(),
        password: BMC_PASSWORD.to_string(),
    }
}

/// A proxy that reaches BMCs through the `bmc_proxy` override `upstream`,
/// whose ACL grants the anonymous caller `acl`, and which holds
/// `credentials` for [`FAKE_BMC_IP`] so no nico-api call is made.
async fn proxy_to(upstream: &str, acl: &str, credentials: BmcCredentials) -> BmcProxyState {
    proxy_configured(upstream, acl, "", credentials).await
}

/// [`proxy_to`], with the `[[class]]` tables `classes`.
async fn proxy_configured(
    upstream: &str,
    acl: &str,
    classes: &str,
    credentials: BmcCredentials,
) -> BmcProxyState {
    let state = test_state_with_config(&format!(
        r#"
        bmc_proxy = "{upstream}"

        [tls]
        identity_pemfile_path = ""
        identity_keyfile_path = ""
        root_cafile_path = ""
        admin_root_cafile_path = ""

        [auth]

        [auth.acls]
        anonymous = {acl}

        {classes}
        "#
    ));
    state
        .credential_cache
        .insert(FAKE_BMC_IP.parse().unwrap(), credentials)
        .await;
    state
}

/// [`proxy_to`] the BMC at `bmc`, overriding only its port, with every
/// request allowed.
async fn proxy_reaching(bmc: SocketAddr, credentials: BmcCredentials) -> BmcProxyState {
    proxy_to(&format!(":{}", bmc.port()), r#"["/**"]"#, credentials).await
}

fn proxied(
    method: Method,
    path: &str,
    forwarded: Option<&str>,
    headers: &[(HeaderName, &str)],
    body: Body,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(forwarded) = forwarded {
        builder = builder.header("forwarded", forwarded);
    }
    for (name, value) in headers {
        builder = builder.header(name.clone(), *value);
    }
    let mut request = builder.body(body).expect("request builds");
    request.extensions_mut().insert(AuthContext::<()> {
        principals: vec![],
        authorization: None,
    });
    request
}

/// A `Forwarded` value naming the fake BMC, among other parameters.
fn to_the_bmc() -> Option<&'static str> {
    Some("for=192.0.2.1;host=127.0.0.1")
}

fn get(path: &str) -> Request<Body> {
    proxied(Method::GET, path, to_the_bmc(), &[], Body::empty())
}

/// What the caller received.
struct Answer {
    status: u16,
    headers: HeaderMap,
    body: Bytes,
}

async fn exchange(state: &BmcProxyState, request: Request<Body>) -> Answer {
    // A request emits metrics that other tests measure, so it is sent inside
    // their serialized window.
    let _metrics = MetricsCapture::start();
    let response = match proxy_request(axum::extract::State(state.clone()), request).await {
        Ok(response) | Err(response) => response,
    };
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("the response body reads");
    Answer {
        status,
        headers,
        body,
    }
}

/// Every value of `name`, so a duplicate is visible.
fn values(headers: &HeaderMap, name: &str) -> Vec<String> {
    headers
        .get_all(name)
        .iter()
        .map(|value| value.to_str().expect("ASCII header").to_string())
        .collect()
}

/// A request reaches the BMC with its query and the caller's own headers,
/// but without the caller's `Forwarded`, which named the BMC to the proxy;
/// the BMC's answer comes back with its headers and body.
#[tokio::test]
async fn a_request_reaches_the_bmc_with_its_query_and_headers() {
    let (addr, bmc) = spawn_fake_bmc();
    let state = proxy_reaching(addr, root_password()).await;
    let answer = exchange(
        &state,
        proxied(
            Method::GET,
            &format!("{SYSTEM_PATH}?$select=PowerState"),
            to_the_bmc(),
            &[(HeaderName::from_static("x-caller-custom"), "passed")],
            Body::empty(),
        ),
    )
    .await;

    assert_eq!(answer.status, 200);
    assert_eq!(answer.body, SYSTEM_BODY);
    assert_eq!(values(&answer.headers, "x-bmc-custom"), ["kept"]);
    assert_eq!(
        values(&answer.headers, "content-type"),
        ["application/json"]
    );

    let [received] = <[Received; 1]>::try_from(bmc.received()).expect("one request");
    assert_eq!(received.method, Method::GET);
    assert_eq!(
        received.path_and_query,
        format!("{SYSTEM_PATH}?$select=PowerState")
    );
    assert_eq!(values(&received.headers, "x-caller-custom"), ["passed"]);
    assert!(values(&received.headers, "forwarded").is_empty());
}

/// With `bmc_proxy` overriding the host, the proxy names the BMC to that
/// host in a `Forwarded` of its own, in place of the caller's.
#[tokio::test]
async fn a_host_override_is_told_the_bmc_in_forwarded() {
    let (addr, bmc) = spawn_fake_bmc();
    let state = proxy_to(&addr.to_string(), r#"["/**"]"#, root_password()).await;
    let answer = exchange(&state, get(SYSTEM_PATH)).await;

    assert_eq!(answer.status, 200);
    let [received] = <[Received; 1]>::try_from(bmc.received()).expect("one request");
    assert_eq!(
        values(&received.headers, "forwarded"),
        [format!("host={FAKE_BMC_IP}")]
    );
}

/// What the BMC saw of the credential: every `Authorization` and every
/// `X-Auth-Token` value.
async fn credential_on_the_wire(credentials: BmcCredentials) -> (Vec<String>, Vec<String>) {
    let (addr, bmc) = spawn_fake_bmc();
    let state = proxy_reaching(addr, credentials).await;
    let answer = exchange(
        &state,
        proxied(
            Method::GET,
            SYSTEM_PATH,
            to_the_bmc(),
            &[
                (header::AUTHORIZATION, "Bearer caller"),
                (HeaderName::from_static("x-auth-token"), "caller-token"),
            ],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(answer.status, 200);
    let [received] = <[Received; 1]>::try_from(bmc.received()).expect("one request");
    (
        values(&received.headers, "authorization"),
        values(&received.headers, "x-auth-token"),
    )
}

/// The proxy's credential replaces whatever the caller sent, in its wire
/// form: a password as HTTP basic authentication, a session in Redfish's
/// token header.
#[tokio::test]
async fn the_proxys_credential_replaces_the_callers() {
    check_cases_async(
        [
            Case {
                scenario: "a password travels as basic authentication",
                input: root_password(),
                expect: Yields((vec![basic(BMC_PASSWORD)], vec![])),
            },
            Case {
                scenario: "a session travels in the Redfish token header",
                input: BmcCredentials::SessionToken {
                    token: "session-1".to_string(),
                },
                expect: Yields((vec![], vec!["session-1".to_string()])),
            },
        ],
        |credentials| async { Ok::<_, Infallible>(credential_on_the_wire(credentials).await) },
    )
    .await;
}

/// How the BMC received a body: (status the caller got, bytes received,
/// `Content-Length` values, `Transfer-Encoding` values).
async fn upload(len: usize) -> (u16, usize, Vec<String>, Vec<String>) {
    let (addr, bmc) = spawn_fake_bmc();
    let state = proxy_reaching(addr, root_password()).await;
    let answer = exchange(
        &state,
        proxied(
            Method::POST,
            UPLOAD_PATH,
            to_the_bmc(),
            &[(header::CONTENT_LENGTH, len.to_string().as_str())],
            Body::from(vec![b'x'; len]),
        ),
    )
    .await;
    let [received] = <[Received; 1]>::try_from(bmc.received()).expect("one request");
    (
        answer.status,
        received.body_len,
        values(&received.headers, "content-length"),
        values(&received.headers, "transfer-encoding"),
    )
}

/// A request body reaches the BMC whole and with its length declared,
/// whether it is small enough to buffer or large enough to stream: BMCs
/// reject chunked uploads.
#[tokio::test]
async fn request_bodies_reach_the_bmc_whole_with_their_length() {
    let streamed = MAX_BUFFERED_BODY_SIZE + 1;
    check_cases_async(
        [
            Case {
                scenario: "a buffered body",
                input: 512,
                expect: Yields((202, 512, vec!["512".to_string()], vec![])),
            },
            Case {
                scenario: "a streamed body",
                input: streamed,
                expect: Yields((202, streamed, vec![streamed.to_string()], vec![])),
            },
        ],
        |len| async move { Ok::<_, Infallible>(upload(len).await) },
    )
    .await;
}

/// A BMC error that echoes the credential the proxy applied reaches the
/// caller without it.
#[tokio::test]
async fn an_error_echoing_the_credential_is_redacted() {
    let (addr, _bmc) = spawn_fake_bmc();
    let state = proxy_reaching(addr, root_password()).await;
    let answer = exchange(&state, get(LEAKY_PATH)).await;

    assert_eq!(answer.status, 500);
    let body = String::from_utf8_lossy(&answer.body);
    assert!(body.contains("rejected password"), "{body}");
    assert!(!body.contains(BMC_PASSWORD), "{body}");
}

struct Rejected {
    method: Method,
    path: &'static str,
    body_len: usize,
}

/// Which credential a request reached the BMC with.
fn credential_sent(received: &Received) -> &'static str {
    let sent = values(&received.headers, "authorization");
    if sent == [basic(BMC_PASSWORD)] {
        "cached"
    } else if sent == [basic(FRESH_PASSWORD)] {
        "fresh"
    } else {
        "other"
    }
}

/// What a request the BMC rejects for its credential leads to: (status the
/// caller got, the password each attempt carried, the password cached
/// afterwards, whether the caller saw either password).
async fn after_rejection(input: Rejected) -> (u16, Vec<&'static str>, Option<String>, bool) {
    let (addr, bmc) = spawn_fake_bmc();
    let mut state = proxy_reaching(addr, root_password()).await;
    state.api_client = fake_nico_api().await;
    let request = if input.body_len == 0 {
        proxied(input.method, input.path, to_the_bmc(), &[], Body::empty())
    } else {
        proxied(
            input.method,
            input.path,
            to_the_bmc(),
            &[(header::CONTENT_LENGTH, input.body_len.to_string().as_str())],
            Body::from(vec![b'x'; input.body_len]),
        )
    };
    let answer = exchange(&state, request).await;

    let attempts = bmc.received().iter().map(credential_sent).collect();
    let cached = match state
        .credential_cache
        .get(&FAKE_BMC_IP.parse::<IpAddr>().unwrap())
        .await
    {
        Some(BmcCredentials::UsernamePassword { password, .. }) => Some(password),
        Some(BmcCredentials::SessionToken { token }) => Some(token),
        None => None,
    };
    let body = String::from_utf8_lossy(&answer.body);
    let leaked = body.contains(BMC_PASSWORD) || body.contains(FRESH_PASSWORD);
    (answer.status, attempts, cached, leaked)
}

/// A credential the BMC rejects is dropped, and a request that can be sent
/// again is sent once more with a fresh one from nico-api. A streamed body
/// is gone after the first attempt, so it is not replayed. A rejection of
/// the fresh credential too drops it. Either way the BMC's echo of a
/// password is scrubbed of the one that attempt sent.
#[tokio::test]
async fn a_rejected_credential_is_replaced_once() {
    check_cases_async(
        [
            Case {
                scenario: "the fresh credential is accepted",
                input: Rejected {
                    method: Method::GET,
                    path: ROTATED_PATH,
                    body_len: 0,
                },
                expect: Yields((
                    200,
                    vec!["cached", "fresh"],
                    Some(FRESH_PASSWORD.to_string()),
                    false,
                )),
            },
            Case {
                scenario: "the fresh credential is rejected too",
                input: Rejected {
                    method: Method::GET,
                    path: REJECTING_PATH,
                    body_len: 0,
                },
                expect: Yields((401, vec!["cached", "fresh"], None, false)),
            },
            Case {
                scenario: "a streamed upload is not replayed",
                input: Rejected {
                    method: Method::POST,
                    path: REJECTING_PATH,
                    body_len: MAX_BUFFERED_BODY_SIZE + 1,
                },
                expect: Yields((401, vec!["cached"], None, false)),
            },
        ],
        |input| async { Ok::<_, Infallible>(after_rejection(input).await) },
    )
    .await;
}

/// A redirect the BMC answers with is followed for a request that can be
/// replayed, and the caller sees the final answer.
#[tokio::test]
async fn a_redirect_is_followed_to_the_final_answer() {
    let (addr, bmc) = spawn_fake_bmc();
    let state = proxy_reaching(addr, root_password()).await;
    let answer = exchange(&state, get(MOVED_PATH)).await;

    assert_eq!(answer.status, 200);
    assert_eq!(answer.body, SYSTEM_BODY);
    let paths: Vec<String> = bmc
        .received()
        .into_iter()
        .map(|received| received.path_and_query)
        .collect();
    assert_eq!(paths, [MOVED_PATH, SYSTEM_PATH]);
}

/// A caller naming its BMC by MAC address reaches the BMC at the IP that
/// address resolves to.
#[tokio::test]
async fn a_bmc_named_by_mac_is_reached_at_its_ip() {
    let (addr, bmc) = spawn_fake_bmc();
    let state = proxy_reaching(addr, root_password()).await;
    state
        .ip_cache
        .insert(
            LookupBy::MacAddress(fake_bmc_mac().to_string()),
            FAKE_BMC_IP.parse().unwrap(),
        )
        .await;
    let answer = exchange(
        &state,
        proxied(
            Method::GET,
            SYSTEM_PATH,
            Some(&format!("mac={}", fake_bmc_mac())),
            &[],
            Body::empty(),
        ),
    )
    .await;

    assert_eq!(answer.status, 200);
    assert_eq!(bmc.received().len(), 1);
}

struct Refused {
    /// The anonymous caller's ACL entries, as a TOML array.
    acl: &'static str,
    request: fn() -> Request<Body>,
    /// Whether anything listens at the BMC's port.
    bmc_listening: bool,
}

/// (status the caller got, whether the BMC received anything).
async fn refusal(input: Refused) -> (u16, bool) {
    let (addr, bmc) = spawn_fake_bmc();
    let (_held, refusing) = refusing_port();
    let port = if input.bmc_listening {
        addr.port()
    } else {
        refusing
    };
    let state = proxy_to(&format!(":{port}"), input.acl, root_password()).await;
    let answer = exchange(&state, (input.request)()).await;
    (answer.status, !bmc.received().is_empty())
}

/// A request the proxy must not or cannot deliver fails with the status
/// that says why, and none reaches the BMC.
#[tokio::test]
async fn requests_the_proxy_does_not_deliver() {
    check_cases_async(
        [
            Case {
                scenario: "the ACL does not grant the method",
                input: Refused {
                    acl: r#"["GET /redfish/v1/**"]"#,
                    request: || {
                        proxied(Method::POST, UPLOAD_PATH, to_the_bmc(), &[], Body::empty())
                    },
                    bmc_listening: true,
                },
                expect: Yields((403, false)),
            },
            Case {
                scenario: "no Forwarded header names a BMC",
                input: Refused {
                    acl: r#"["/**"]"#,
                    request: || proxied(Method::GET, SYSTEM_PATH, None, &[], Body::empty()),
                    bmc_listening: true,
                },
                expect: Yields((400, false)),
            },
            Case {
                scenario: "the Forwarded host is not an IP",
                input: Refused {
                    acl: r#"["/**"]"#,
                    request: || {
                        proxied(
                            Method::GET,
                            SYSTEM_PATH,
                            Some("host=bmc.example"),
                            &[],
                            Body::empty(),
                        )
                    },
                    bmc_listening: true,
                },
                expect: Yields((400, false)),
            },
            Case {
                scenario: "a body too large to buffer declares no length",
                input: Refused {
                    acl: r#"["/**"]"#,
                    request: || {
                        proxied(
                            Method::POST,
                            UPLOAD_PATH,
                            to_the_bmc(),
                            &[],
                            Body::from(vec![b'x'; MAX_BUFFERED_BODY_SIZE + 1]),
                        )
                    },
                    bmc_listening: true,
                },
                expect: Yields((400, false)),
            },
            Case {
                scenario: "nothing answers at the BMC",
                input: Refused {
                    acl: r#"["/**"]"#,
                    request: || get(SYSTEM_PATH),
                    bmc_listening: false,
                },
                expect: Yields((502, false)),
            },
        ],
        |input| async { Ok::<_, Infallible>(refusal(input).await) },
    )
    .await;
}

/// A class for `GET`s of the slow paths, and of [`ROTATED_PATH`], whose
/// replay can wait on nico-api. Its budget is far shorter than
/// [`SLOW_ANSWER_DELAY`], and long enough for an attempt to reach the BMC
/// even on a loaded machine.
const QUICK_CLASS: &str = r#"
    [[class]]
    name = "quick"
    match = [
        "GET /redfish/v1/Slow",
        "GET /redfish/v1/SlowRotated",
        "GET /redfish/v1/SlowBody",
        "GET /redfish/v1/Rotated",
    ]
    upstream_timeout = "2s"
"#;

/// How nico-api answers the proxy's lookups of the BMC's credentials.
#[derive(Clone, Copy)]
enum Lookup {
    /// At once, the proxy holding the BMC's credential already.
    Prompt,
    /// After [`SLOW_ANSWER_DELAY`], the proxy holding the BMC's credential
    /// already.
    SlowToReplace,
    /// After [`SLOW_ANSWER_DELAY`], the proxy holding no credential for the
    /// BMC.
    SlowToFetch,
}

/// What a request for `method` on `path` gets from a proxy with
/// [`QUICK_CLASS`], when nico-api answers its lookups as `lookup` says:
/// (status the caller got, whether the body arrived "whole" or was "cut
/// off", the credential each attempt at the BMC carried). The caller must
/// get its answer long before the slow BMC or nico-api would have answered.
async fn under_the_quick_class(
    (method, path, lookup): (Method, &'static str, Lookup),
) -> (u16, &'static str, Vec<&'static str>) {
    let (addr, bmc) = spawn_fake_bmc();
    let mut state = proxy_configured(
        &format!(":{}", addr.port()),
        r#"["/**"]"#,
        QUICK_CLASS,
        root_password(),
    )
    .await;
    let lookup_delay = match lookup {
        Lookup::Prompt => Duration::ZERO,
        Lookup::SlowToReplace => SLOW_ANSWER_DELAY,
        Lookup::SlowToFetch => {
            state.credential_cache.invalidate_all();
            SLOW_ANSWER_DELAY
        }
    };
    state.api_client = fake_nico_api_answering_after(lookup_delay).await;

    // The request emits metrics that other tests measure, so it is sent
    // inside their serialized window.
    let _metrics = MetricsCapture::start();
    let request = proxied(method, path, to_the_bmc(), &[], Body::empty());
    let response = tokio::time::timeout(
        SLOW_ANSWER_DELAY / 2,
        proxy_request(axum::extract::State(state.clone()), request),
    )
    .await
    .expect("the budget ends the request long before the slow side answers");
    let response = match response {
        Ok(response) | Err(response) => response,
    };
    let status = response.status().as_u16();
    let body = match axum::body::to_bytes(response.into_body(), usize::MAX).await {
        Ok(_) => "whole",
        Err(_) => "cut off",
    };
    let attempts = bmc.received().iter().map(credential_sent).collect();
    (status, body, attempts)
}

/// A request is held to its class's upstream budget, response body, replay
/// with fresh credentials, and nico-api's credential lookups included.
#[tokio::test]
async fn a_request_is_held_to_its_classs_budget() {
    check_cases_async(
        [
            Case {
                scenario: "the BMC answers after the budget, classified by path alone",
                input: (
                    Method::GET,
                    "/redfish/v1/Slow?$select=PowerState",
                    Lookup::Prompt,
                ),
                expect: Yields((502, "whole", vec!["cached"])),
            },
            Case {
                scenario: "the replay with fresh credentials is answered after the budget",
                input: (Method::GET, SLOW_ROTATED_PATH, Lookup::Prompt),
                expect: Yields((502, "whole", vec!["cached", "fresh"])),
            },
            Case {
                scenario: "the body is still streaming when the budget runs out",
                input: (Method::GET, SLOW_BODY_PATH, Lookup::Prompt),
                expect: Yields((200, "cut off", vec!["cached"])),
            },
            Case {
                scenario: "nico-api hands out the credentials after the budget",
                input: (Method::GET, SLOW_PATH, Lookup::SlowToFetch),
                expect: Yields((502, "whole", vec![])),
            },
            Case {
                scenario: "nico-api hands out fresh credentials after the budget",
                input: (Method::GET, ROTATED_PATH, Lookup::SlowToReplace),
                expect: Yields((502, "whole", vec!["cached"])),
            },
        ],
        |input| async move { Ok::<_, Infallible>(under_the_quick_class(input).await) },
    )
    .await;
}

/// Keeps tracing's process-wide callsite cache interested in every span, as
/// `carbide_instrument::testing` does for its captures. Without it, while a
/// test's thread-local subscriber is the only one alive, other threads can
/// cache the request span's callsite as unwanted, and the span is never
/// created.
fn keep_callsites_enabled() {
    static DISPATCH: OnceLock<tracing::Dispatch> = OnceLock::new();
    DISPATCH.get_or_init(|| tracing::Dispatch::new(tracing_subscriber::registry()));
}

/// What a request for `method` on [`SLOW_PATH`] that names no BMC gets from a
/// proxy with [`QUICK_CLASS`]: (status the caller got, the class its trace
/// span names). The proxy refuses the request right after classifying it,
/// so no task outlives the capture: a span such a task closed after the
/// capture ended would reach a subscriber that never saw it.
async fn class_on_the_span(method: Method) -> (u16, String) {
    keep_callsites_enabled();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let _traced = tracing::subscriber::set_default(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test"))),
    );
    let state = proxy_configured(":1", r#"["/**"]"#, QUICK_CLASS, root_password()).await;
    let answer = exchange(&state, proxied(method, SLOW_PATH, None, &[], Body::empty())).await;
    let span = exporter
        .get_finished_spans()
        .expect("finished spans")
        .into_iter()
        .find(|span| span.name == "bmc_proxy_request")
        .expect("the request span is exported");
    let class = span
        .attributes
        .into_iter()
        .find(|attribute| attribute.key.as_str() == "bmc_proxy.class")
        .map_or_else(
            || "(none)".to_string(),
            |attribute| attribute.value.to_string(),
        );
    (answer.status, class)
}

/// A request's trace span names the class it was classified into.
#[tokio::test]
async fn the_request_span_names_its_class() {
    check_cases_async(
        [
            Case {
                scenario: "a class pattern matches",
                input: Method::GET,
                expect: Yields((400, "quick".to_string())),
            },
            Case {
                scenario: "no class pattern matches",
                input: Method::POST,
                expect: Yields((400, "default".to_string())),
            },
        ],
        |method| async move { Ok::<_, Infallible>(class_on_the_span(method).await) },
    )
    .await;
}

/// The answer to `request`, its body not yet read. Unlike [`exchange`], it
/// takes no [`MetricsCapture`]: a test that holds slots takes one before its
/// first request and sends every request within it. Waiting for the capture
/// blocks the test's thread, and a slot held meanwhile could outlive its
/// exchange's bound and be reclaimed.
async fn answer(state: &BmcProxyState, request: Request<Body>) -> http::Response<Body> {
    match proxy_request(axum::extract::State(state.clone()), request).await {
        Ok(response) | Err(response) => response,
    }
}

/// A class of one request at a time at its BMC, with a short budget.
const ONE_AT_A_TIME: &str = r#"
    [[class]]
    name = "one_at_a_time"
    match = ["GET /redfish/v1/Systems/System_0"]
    max_in_flight = 1
    upstream_timeout = "2s"
"#;

/// A request holds its slot at its BMC until its response body has been
/// sent. While one's body is unread, the next request of a class of one at a
/// time waits, never reaching the BMC, and is refused with 503 when its
/// budget runs out; once that body is gone, the next request goes through at
/// once, long before the held slot's bound would have reclaimed it.
#[tokio::test]
async fn a_slot_is_held_until_the_response_body_is_sent() {
    let _metrics = MetricsCapture::start();
    let (addr, bmc) = spawn_fake_bmc();
    let state = proxy_configured(
        &format!(":{}", addr.port()),
        r#"["/**"]"#,
        ONE_AT_A_TIME,
        root_password(),
    )
    .await;
    let unread = answer(&state, get(SYSTEM_PATH)).await;
    // The class's budget bounds the wait.
    let waited_out = answer(&state, get(SYSTEM_PATH)).await.status().as_u16();
    let reached_while_unread = bmc.received().len();
    drop(unread);
    let sent_at = tokio::time::Instant::now();
    let after = answer(&state, get(SYSTEM_PATH)).await.status().as_u16();
    let after_at_once = sent_at.elapsed() < Duration::from_secs(1);

    assert_eq!(
        (
            waited_out,
            reached_while_unread,
            after,
            after_at_once,
            bmc.received().len()
        ),
        (503, 1, 200, true, 2),
    );
}

/// What a request for `path` gets from a class of one at a time with a
/// 3-second budget, once it has waited 1.5 seconds for its slot.
async fn after_waiting_for_a_slot(path: &'static str) -> u16 {
    let _metrics = MetricsCapture::start();
    let (addr, _bmc) = spawn_fake_bmc();
    let mut state = proxy_configured(
        &format!(":{}", addr.port()),
        r#"["/**"]"#,
        &format!(
            r#"
            [[class]]
            name = "one_at_a_time"
            match = ["GET {SYSTEM_PATH}", "GET {BRIEFLY_SLOW_PATH}", "GET {BRIEFLY_SLOW_ROTATED_PATH}"]
            max_in_flight = 1
            upstream_timeout = "3s"
            "#
        ),
        root_password(),
    )
    .await;
    state.api_client = fake_nico_api().await;
    let unread = answer(&state, get(SYSTEM_PATH)).await;
    let waiting = answer(&state, get(path));
    let release = async {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        drop(unread);
    };
    let (response, ()) = tokio::join!(waiting, release);
    response.status().as_u16()
}

/// Time a request spends waiting for its slot comes out of its first
/// attempt's budget, and not out of a replay's with fresh credentials. With
/// 1.5 of its 3 seconds left, a BMC that answers in 2.5 seconds is cut off,
/// unless it answers a replay.
#[tokio::test]
async fn waiting_for_a_slot_spends_the_first_attempts_budget() {
    check_cases_async(
        [
            Case {
                scenario: "the first attempt has what the wait left",
                input: BRIEFLY_SLOW_PATH,
                expect: Yields(502),
            },
            Case {
                scenario: "a replay has the whole budget",
                input: BRIEFLY_SLOW_ROTATED_PATH,
                expect: Yields(200),
            },
        ],
        |path| async move { Ok::<_, Infallible>(after_waiting_for_a_slot(path).await) },
    )
    .await;
}

/// A request replayed with fresh credentials keeps its slot: while the
/// replay's answer is unread, the next request of a class of one at a time
/// waits and is refused, and the BMC has received only the two attempts.
#[tokio::test]
async fn a_replay_keeps_its_slot() {
    let _metrics = MetricsCapture::start();
    let (addr, bmc) = spawn_fake_bmc();
    let mut state = proxy_configured(
        &format!(":{}", addr.port()),
        r#"["/**"]"#,
        &format!(
            r#"
            [[class]]
            name = "one_at_a_time"
            match = ["GET {SYSTEM_PATH}", "GET {ROTATED_PATH}"]
            max_in_flight = 1
            upstream_timeout = "2s"
            "#
        ),
        root_password(),
    )
    .await;
    state.api_client = fake_nico_api().await;
    let unread = answer(&state, get(ROTATED_PATH)).await;
    let unread_status = unread.status().as_u16();
    let waited_out = answer(&state, get(SYSTEM_PATH)).await.status().as_u16();
    let reached_while_unread = bmc.received().len();
    drop(unread);
    assert_eq!(
        (unread_status, waited_out, reached_while_unread),
        (200, 503, 2)
    );
}
