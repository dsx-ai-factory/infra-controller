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

//! The BMC proxy: authenticated callers name a BMC, and the proxy forwards
//! their Redfish request to it with credentials it resolves from nico-api.
//!
//! The modules follow a request's path:
//!
//! - `ingress`: accepts connections over TLS. The proxy's own identity and
//!   trusted CAs are reloaded from disk on the first connection after five
//!   minutes. A client certificate is optional here.
//! - `guard`: identifies the caller from its certificate, then checks the
//!   principal allow-list and the per-principal ACL.
//! - `target`: resolves the BMC the `Forwarded` header names to its IP.
//! - `admission`: holds a request until its BMC has a slot for it, when its
//!   class or the per-BMC limit caps how many requests the BMC receives.
//! - `credentials`: the BMC's credentials, fetched from nico-api and cached
//!   by IP.
//! - `upstream`: sends the request to the BMC with the caller's headers and
//!   body, resolving credentials for each attempt.
//! - `response`: builds the caller's answer. A 4xx or 5xx body is scrubbed
//!   of the credential its attempt used, or replaced when it cannot be
//!   inspected; other bodies stream through unchanged.
//!
//! [`proxy_request_inner`] runs a request from the ACL on. When the BMC
//! answers 401 or 403 and the body can be sent again, it drops the cached
//! credentials and sends the request once more with fresh ones.

mod admission;
mod credentials;
#[cfg(test)]
mod end_to_end_tests;
mod guard;
mod ingress;
mod response;
mod target;
#[cfg(test)]
mod test_support;
mod upstream;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::any;
use carbide_instrument::emit;
use forge_tls::client_config::ClientCert;
use http::{Method, Request, Response, StatusCode};
use moka::future::Cache as MokaCache;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use trace_propagation::set_span_parent_from_headers;
use tracing::Instrument;

use crate::metrics::{MethodLabel, UpstreamAuthRetried};
use crate::proxy::admission::Admission;
use crate::proxy::credentials::{
    CREDENTIAL_CACHE_IDLE_TTL, CredentialCache, evict_cached_credentials, get_bmc_credentials,
};
use crate::proxy::guard::{authorize_proxy_request, cert_description_layer};
use crate::proxy::ingress::{BmcProxy, RefreshableTlsAcceptor};
use crate::proxy::response::{BmcOrigins, build_response, prepare_response_body};
use crate::proxy::target::{
    IP_CACHE_TTL, LookupToIpCache, forwarded_header_value, ip_for_forwarded_target,
};
use crate::proxy::upstream::{
    UpstreamBody, UpstreamResponse, build_http_client, method_supports_body, send_upstream,
};

#[derive(thiserror::Error, Debug)]
pub(crate) enum BmcProxyError {
    #[error("error resolving BMC information through carbide API: {0}")]
    Api(String),
    #[error("invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("internal error proxying request: {0}")]
    InternalProxying(String),
    #[error("no credentials found for BMC IP address: {0}")]
    NoCredentials(IpAddr),
    #[error("error spawning listener: {0}")]
    Listen(std::io::Error),
    #[error("error loading TLS config: {0}")]
    TlsConfig(String),
}

pub(crate) struct BmcProxyParams {
    pub(crate) config: Arc<crate::Config>,
}

#[derive(Clone)]
struct BmcProxyState {
    config: Arc<crate::Config>,
    api_client: ForgeApiClient,
    credential_cache: CredentialCache,
    /// One client for every upstream: reqwest pools connections per host
    /// internally, so per-BMC clients bought nothing and grew without bound.
    http_client: reqwest_middleware::ClientWithMiddleware,
    ip_cache: LookupToIpCache,
    admission: Arc<Admission>,
}

/// Upper bound on cached entries; sized far above any realistic BMC fleet.
const CACHE_MAX_ENTRIES: u64 = 100_000;

fn bounded_cache<K, V>(ttl: Duration) -> MokaCache<K, V>
where
    K: std::hash::Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    MokaCache::builder()
        .time_to_live(ttl)
        .max_capacity(CACHE_MAX_ENTRIES)
        .build()
}

/// Like [`bounded_cache`], but expiry counts from last use rather than from
/// insertion, so entries under active traffic never expire.
fn idle_bounded_cache<K, V>(idle_ttl: Duration) -> MokaCache<K, V>
where
    K: std::hash::Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    MokaCache::builder()
        .time_to_idle(idle_ttl)
        .max_capacity(CACHE_MAX_ENTRIES)
        .build()
}

pub(crate) async fn start(
    params: BmcProxyParams,
    cancel_token: CancellationToken,
    join_set: &mut JoinSet<()>,
) -> Result<(), BmcProxyError> {
    // Destructure params to save typing
    let BmcProxyParams { config } = params;

    tracing::info!(
        listen_address = config.listen.to_string(),
        build_version = carbide_version::v!(build_version),
        build_date = carbide_version::v!(build_date),
        rust_version = carbide_version::v!(rust_version),
        "Start carbide BMC proxy",
    );

    let listener = crate::net::bind_with_ipv4_fallback(config.listen)
        .await
        .map_err(BmcProxyError::Listen)?;

    let client_config = ForgeClientConfig::new(
        config.carbide_api.root_ca.clone(),
        Some(ClientCert {
            cert_path: config.carbide_api.client_cert.clone(),
            key_path: config.carbide_api.client_key.clone(),
        }),
    );
    let api_config = ApiConfig::new(config.carbide_api.api_url.as_str(), &client_config);
    let api_client = ForgeApiClient::new(&api_config);

    let admission = Admission::start(
        &config.classes,
        &config.admission,
        cancel_token.clone(),
        join_set,
    );
    let state = BmcProxyState {
        http_client: build_http_client(config.redirects.mode)?,
        config,
        api_client,
        credential_cache: idle_bounded_cache(CREDENTIAL_CACHE_IDLE_TTL),
        ip_cache: bounded_cache(IP_CACHE_TTL),
        admission,
    };

    let app = proxy_routes(state.clone())
        .layer(from_fn_with_state(state.clone(), authorize_proxy_request))
        .layer(cert_description_layer::<()>(&state.config.auth)?);

    let tls_acceptor = RefreshableTlsAcceptor::new(state.config.tls.clone()).await?;

    let bmc_proxy = BmcProxy {
        app,
        listener,
        state,
        tls_acceptor,
    };

    join_set
        .build_task()
        .name("bmc-proxy listener")
        .spawn(bmc_proxy.run(cancel_token))
        // Safety: will only fail if outside tokio runtime
        .expect("Error spawning bmc-proxy listener");

    Ok(())
}

/// Builds the production route table before transport authorization layers are
/// applied.
fn proxy_routes(state: BmcProxyState) -> Router {
    Router::new()
        .route("/", any(root_or_proxy))
        .route("/{*path}", any(proxy_request))
        .with_state(state)
}

/// Returns the build banner served by an untargeted `GET /`.
fn root_url() -> &'static str {
    const ROOT_CONTENTS: &str = if carbide_version::literal!(build_version).is_empty() {
        "Carbide BMC proxy development build\n"
    } else {
        concat!(
            "Carbide BMC proxy ",
            carbide_version::literal!(build_version),
            "\n"
        )
    };
    ROOT_CONTENTS
}

/// Serves the proxy banner only when `/` is not targeted at a BMC.
///
/// A returned same-BMC redirect can legitimately name `/`; a request carrying
/// `Forwarded` must therefore enter the proxy path and receive normal ACL
/// handling. Malformed `Forwarded` values also fail closed in that path.
async fn root_or_proxy(
    State(state): State<BmcProxyState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if request.headers().contains_key("forwarded") {
        return proxy_request(State(state), request).await;
    }
    if request.method() == Method::GET {
        return Ok(root_url().into_response());
    }
    if request.method() == Method::HEAD {
        let mut response = root_url().into_response();
        *response.body_mut() = Body::empty();
        return Ok(response);
    }

    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response.headers_mut().insert(
        http::header::ALLOW,
        http::HeaderValue::from_static("GET,HEAD"),
    );
    Ok(response)
}

async fn proxy_request(
    State(state): State<BmcProxyState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let request_span = bmc_proxy_request_span(&request);

    let result = proxy_request_inner(state, request)
        .instrument(request_span.clone())
        .await;
    let status = match &result {
        Ok(response) | Err(response) => response.status(),
    };
    request_span.record("http.response.status_code", status.as_u16());
    request_span.record("otel.status_code", span_status(status));
    result
}

fn bmc_proxy_request_span<B>(request: &Request<B>) -> tracing::Span {
    let request_span = tracing::info_span!(
        parent: None,
        "bmc_proxy_request",
        http.request.method = %request.method(),
        url.path = %request.uri().path(),
        http.response.status_code = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
        bmc.ip_address = tracing::field::Empty,
        bmc_proxy.class = tracing::field::Empty,
        logfmt.suppress = true,
    );
    set_span_parent_from_headers(&request_span, request.headers());
    request_span
}

/// The OpenTelemetry status for a proxied request that answered with `status`.
///
/// Only a 5xx marks the span failed: a rejected or malformed request is the caller's error, and
/// counting it against the proxy would bury the hops that actually broke.
fn span_status(status: StatusCode) -> &'static str {
    if status.is_server_error() {
        "error"
    } else {
        "ok"
    }
}

async fn proxy_request_inner(
    state: BmcProxyState,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    if !state.allows(&request) {
        return Ok(error_response((StatusCode::FORBIDDEN, "Forbidden").into()));
    }
    let class = state
        .config
        .classes
        .classify(request.method(), request.uri().path());
    tracing::Span::current().record("bmc_proxy.class", class.name.as_str());
    let (parts, body) = request.into_parts();
    let forwarded_target = forwarded_header_value(&parts.headers)
        .map_err(|e| error_response((StatusCode::BAD_REQUEST, e.to_string()).into()))?
        .ok_or_else(|| {
            error_response(
                (
                    StatusCode::BAD_REQUEST,
                    "missing Forwarded host/mac/serial in request header",
                )
                    .into(),
            )
        })?;

    let target_ip = match ip_for_forwarded_target(&forwarded_target, &state).await {
        Ok(Some(ip)) => ip,
        Ok(None) => {
            return Err(error_response(
                (
                    StatusCode::BAD_REQUEST,
                    "Could not find BMC from forwarded header",
                )
                    .into(),
            ));
        }
        Err(e) => {
            return Err(error_response(
                (
                    StatusCode::BAD_GATEWAY,
                    format!("Failure looking up BMC IP from target: {e}"),
                )
                    .into(),
            ));
        }
    };

    tracing::Span::current().record("bmc.ip_address", target_ip.to_string());

    let path_and_query = parts
        .uri
        .clone()
        .into_parts()
        .path_and_query
        .ok_or_else(|| error_response((StatusCode::BAD_REQUEST, "missing path").into()))?;

    // Buffer a replayable body once so a stale-credential rejection can be
    // retried; streamed (large) bodies are sent as-is and never replayed.
    let mut upstream_body = if method_supports_body(&parts.method) {
        UpstreamBody::prepare(&parts.headers, body)
            .await
            .map_err(|e| error_response((StatusCode::BAD_REQUEST, e.to_string()).into()))?
    } else {
        UpstreamBody::None
    };

    // The first attempt's budget also covers resolving the BMC's
    // credentials and waiting for a slot there. Resolving them first means
    // only a BMC nico-api knows takes a slot, and the lookup holds none.
    let deadline = tokio::time::Instant::now() + class.upstream_timeout;
    tokio::time::timeout_at(
        deadline,
        get_bmc_credentials(target_ip, &state.api_client, &state.credential_cache),
    )
    .await
    .map_err(|_elapsed| {
        error_response(
            (
                StatusCode::BAD_GATEWAY,
                "timed out resolving the BMC's credentials",
            )
                .into(),
        )
    })?
    .map_err(|e| error_response((StatusCode::BAD_GATEWAY, e.to_string()).into()))?;
    let slot = state
        .admission
        .acquire(
            target_ip,
            class,
            deadline,
            upstream_body.exchange_bound(class.upstream_timeout),
        )
        .await
        .map_err(|refused| {
            error_response((StatusCode::SERVICE_UNAVAILABLE, refused.to_string()).into())
        })?;
    let mut upstream_response = send_upstream(
        &state,
        target_ip,
        &parts,
        path_and_query.clone(),
        &mut upstream_body,
        deadline,
    )
    .await?;

    // A BMC that rejects the credential the proxy cached (an expired Redfish
    // session, a rotated password) gets one replay with freshly resolved
    // credentials, so callers never see a stale-session 401. Only replayable
    // bodies qualify; a streamed body was consumed by the first attempt.
    let rejected = |status: reqwest::StatusCode| {
        status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
    };
    if rejected(upstream_response.response.status()) && upstream_body.is_replayable() {
        evict_cached_credentials(target_ip, &state.credential_cache).await;
        emit(UpstreamAuthRetried {
            method: MethodLabel::from(&parts.method),
            bmc_ip_address: target_ip.to_string(),
        });
        upstream_response = send_upstream(
            &state,
            target_ip,
            &parts,
            path_and_query,
            &mut upstream_body,
            tokio::time::Instant::now() + class.upstream_timeout,
        )
        .await?;
    }

    let UpstreamResponse {
        response,
        sensitive_values,
    } = upstream_response;
    let status = response.status();
    let headers = response.headers().clone();
    let origins = BmcOrigins::new(response.url().clone(), target_ip);
    let body = prepare_response_body(
        status,
        &headers,
        Body::from_stream(response.bytes_stream()),
        &sensitive_values,
    )
    .await;

    if rejected(status) {
        evict_cached_credentials(target_ip, &state.credential_cache).await;
    }

    Ok(build_response(
        status,
        &headers,
        body,
        &origins,
        &parts.method,
        state.config.redirects.mode,
        &sensitive_values,
    )
    .map(|body| slot.hold_until_sent(body)))
}

fn error_response(error: ProxyError) -> Response<Body> {
    (error.status, error.message).into_response()
}

#[derive(Debug)]
struct ProxyError {
    status: StatusCode,
    message: String,
}

impl From<(StatusCode, String)> for ProxyError {
    fn from((status, message): (StatusCode, String)) -> Self {
        Self { status, message }
    }
}

impl From<(StatusCode, &'static str)> for ProxyError {
    fn from((status, message): (StatusCode, &'static str)) -> Self {
        Self {
            status,
            message: message.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{HeaderValue, Method, Request, StatusCode};
    use carbide_authn::middleware::{AuthContext, Principal};
    use carbide_test_support::value_scenarios;
    use tower::ServiceExt;

    use super::{bmc_proxy_request_span, proxy_routes, span_status};
    use crate::proxy::test_support::test_state_with_config;

    const ROOT_ROUTE_TEST_CONFIG: &str = r#"
        allowed_principals = ["spiffe-service-id/forge-system/carbide-api"]

        [tls]
        identity_pemfile_path = ""
        identity_keyfile_path = ""
        root_cafile_path = ""
        admin_root_cafile_path = ""

        [auth]

        [auth.acls]
        "spiffe-service-id/forge-system/carbide-api" = ["GET /**"]
    "#;

    #[tokio::test]
    async fn root_dispatches_targeted_requests_to_the_proxy() {
        let state = test_state_with_config(ROOT_ROUTE_TEST_CONFIG);
        let app = proxy_routes(state);

        let banner = Request::builder()
            .method(Method::GET)
            .uri("/")
            .body(Body::empty())
            .expect("banner request builds");
        assert_eq!(
            app.clone().oneshot(banner).await.unwrap().status(),
            StatusCode::OK
        );

        let mut targeted = Request::builder()
            .method(Method::GET)
            .uri("/")
            .header("forwarded", "host=not-an-ip-address")
            .body(Body::empty())
            .expect("targeted request builds");
        targeted.extensions_mut().insert(AuthContext::<()> {
            principals: vec![Principal::SpiffeServiceIdentifier(
                "forge-system/carbide-api".to_string(),
            )],
            authorization: None,
        });
        assert_eq!(
            app.clone().oneshot(targeted).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );

        let post = Request::builder()
            .method(Method::POST)
            .uri("/")
            .body(Body::empty())
            .expect("POST request builds");
        let post = app.oneshot(post).await.unwrap();
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            post.headers().get(http::header::ALLOW),
            Some(&HeaderValue::from_static("GET,HEAD"))
        );
    }

    #[test]
    fn proxy_request_span_continues_inbound_trace_on_upstream_inject() {
        use opentelemetry::trace::{SpanId, TraceContextExt, TraceId, TracerProvider};
        use opentelemetry_sdk::propagation::TraceContextPropagator;
        use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider};
        use trace_propagation::{extract_context, inject_current_context};
        use tracing_subscriber::layer::SubscriberExt;

        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("nico-bmc-proxy-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));

        let inbound_trace = 0x42u128;
        let inbound_span = 0x55u64;
        let mut inbound_headers = http::HeaderMap::new();
        inbound_headers.insert(
            "traceparent",
            format!("00-{:032x}-{:016x}-01", inbound_trace, inbound_span)
                .parse()
                .unwrap(),
        );

        let mut egress_headers = http::HeaderMap::new();
        tracing::subscriber::with_default(subscriber, || {
            let request = Request::builder()
                .uri("/redfish/v1/Systems")
                .header("traceparent", inbound_headers["traceparent"].clone())
                .body(())
                .unwrap();
            let request_span = bmc_proxy_request_span(&request);
            let _entered = request_span.enter();
            inject_current_context(&mut egress_headers);
        });

        let egress_context = extract_context(&egress_headers);
        assert_eq!(
            egress_context.span().span_context().trace_id(),
            TraceId::from(inbound_trace),
        );
        assert_ne!(
            egress_context.span().span_context().span_id(),
            SpanId::from(inbound_span),
        );

        let spans = exporter.get_finished_spans().expect("finished spans");
        let request = spans
            .iter()
            .find(|span| span.name == "bmc_proxy_request")
            .expect("request span exported");
        assert_eq!(
            request.span_context.trace_id(),
            TraceId::from(inbound_trace)
        );
        assert_eq!(request.parent_span_id, SpanId::from(inbound_span));
    }

    #[test]
    fn proxy_request_span_reports_only_server_errors_as_failed() {
        value_scenarios!(
            run = span_status;
            "success" {
                StatusCode::OK => "ok",
            }

            "redirect" {
                StatusCode::TEMPORARY_REDIRECT => "ok",
            }

            "rejected by the allow list" {
                StatusCode::FORBIDDEN => "ok",
            }

            "malformed request" {
                StatusCode::BAD_REQUEST => "ok",
            }

            "upstream unreachable" {
                StatusCode::BAD_GATEWAY => "error",
            }

            "proxy failure" {
                StatusCode::INTERNAL_SERVER_ERROR => "error",
            }
        );
    }
}
