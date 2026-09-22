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

//! IMDS-style `GET …/meta-data/identity` (shared between carbide-agent and carbide-fmds).
//!
//! Defaults and numeric bounds are in [`forge_dpu_agent_utils::machine_identity`] (`defaults`, `limits`;
//! the latter is also used by carbide-host-support validation).

use std::convert::TryFrom;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::http::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use carbide_instrument::{Event, LabelValue, emit};
use forge_dpu_agent_utils::machine_identity::defaults::{
    BURST, REQUESTS_PER_SECOND, SIGN_TIMEOUT_SECS, WAIT_TIMEOUT_SECS,
};
use forge_dpu_agent_utils::machine_identity::limits::{
    BURST_MAX, BURST_MIN, REQUESTS_PER_SECOND_MAX, REQUESTS_PER_SECOND_MIN, SIGN_TIMEOUT_SECS_MAX,
    SIGN_TIMEOUT_SECS_MIN, WAIT_TIMEOUT_SECS_MAX, WAIT_TIMEOUT_SECS_MIN,
};
use governor::middleware::NoOpMiddleware;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter, clock};
use rpc::fmds::FmdsMachineIdentityConfig;
use rpc::forge::MachineIdentityResponse;

/// `meta-data` leaf name for machine identity (`…/meta-data/identity`).
pub const META_DATA_IDENTITY_CATEGORY: &str = "identity";

/// Upstream path appended to `sign-proxy-url` for HTTP pass-through (`{base}/latest/...`).
pub const SIGN_PROXY_UPSTREAM_IMDS_PREFIX: &str = "latest/meta-data/identity";

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum SignProxyFailureStage {
    RequestTimeout,
    RequestTransport,
    ResponseBody,
    ResponseBuild,
    UpstreamServer,
}

/// A machine-identity sign-proxy call did not produce a usable response. Each
/// variant is one place it breaks, and holds only what that place has.
#[derive(Event)]
#[event(
    event_name = "machine_identity_sign_proxy_failed",
    metric_name = "carbide_machine_identity_sign_proxy_failures_total",
    component = "machine-identity",
    metric = counter,
    describe = "Number of machine identity sign proxy transport, local response handling, and upstream server failures, by failure stage.",
    labels(failure_stage: SignProxyFailureStage),
)]
enum MachineIdentitySignProxyFailed {
    #[event(
        labels(failure_stage = SignProxyFailureStage::RequestTimeout),
        log = warn,
        message = "machine identity sign proxy request failed"
    )]
    RequestTimeout {
        #[context]
        error: String,
    },

    #[event(
        labels(failure_stage = SignProxyFailureStage::RequestTransport),
        log = warn,
        message = "machine identity sign proxy request failed"
    )]
    RequestTransport {
        #[context]
        error: String,
    },

    #[event(
        labels(failure_stage = SignProxyFailureStage::ResponseBody),
        log = warn,
        message = "machine identity sign proxy response body read failed"
    )]
    ResponseBody {
        #[context]
        error: String,
    },

    #[event(
        labels(failure_stage = SignProxyFailureStage::ResponseBuild),
        log = warn,
        message = "machine identity sign proxy response build failed"
    )]
    ResponseBuild {
        #[context]
        error: String,
    },

    #[event(
        labels(failure_stage = SignProxyFailureStage::UpstreamServer),
        log = off
    )]
    UpstreamServer {},
}
/// Validated, normalized machine-identity limits.
///
/// Construct only through [`Self::try_from_limits`], [`TryFrom`] from `FmdsMachineIdentityConfig`,
/// or [`Default`] (known-good defaults, aligned with agent `MachineIdentityConfig` serde).
///
/// ## What is validated where
///
/// - [`MachineIdentityParams::try_from_limits`] and [`TryFrom`] from [`FmdsMachineIdentityConfig`]:
///   numeric ranges, trim/empty normalization for proxy URL and TLS root CA path, and the rule that a CA
///   path requires a proxy URL.
/// - **Agent `MachineIdentityConfig::validate()`** (carbide-host-support): the above bounds **plus** HTTP(S)
///   scheme checks for `sign-proxy-url`, PEM file readability/parsing for `sign-proxy-tls-root-ca`, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineIdentityParams {
    pub requests_per_second: u8,
    pub burst: u8,
    pub wait_timeout_secs: u8,
    pub sign_timeout_secs: u8,
    pub sign_proxy_url: Option<String>,
    pub sign_proxy_tls_root_ca: Option<String>,
}

impl Default for MachineIdentityParams {
    /// Matches [`forge_dpu_agent_utils::machine_identity::defaults`] (agent `MachineIdentityConfig`
    /// serde and this crate).
    fn default() -> Self {
        Self {
            requests_per_second: REQUESTS_PER_SECOND,
            burst: BURST,
            wait_timeout_secs: WAIT_TIMEOUT_SECS,
            sign_timeout_secs: SIGN_TIMEOUT_SECS,
            sign_proxy_url: None,
            sign_proxy_tls_root_ca: None,
        }
    }
}

impl MachineIdentityParams {
    /// Single normalization path: range checks (see [`limits`]), trim,
    /// empty→`None`, and CA path requires proxy URL.
    ///
    /// Call after agent **`MachineIdentityConfig::validate()`** for file-backed config, or use
    /// [`TryFrom`] with `&FmdsMachineIdentityConfig` and [`From`] / `.into()` into [`FmdsMachineIdentityConfig`] for the FMDS boundary.
    pub fn try_from_limits(
        requests_per_second: u8,
        burst: u8,
        wait_timeout_secs: u8,
        sign_timeout_secs: u8,
        sign_proxy_url: Option<&str>,
        sign_proxy_tls_root_ca: Option<&str>,
    ) -> Result<Self, String> {
        if !(REQUESTS_PER_SECOND_MIN..=REQUESTS_PER_SECOND_MAX).contains(&requests_per_second) {
            return Err(format!(
                "machine-identity.requests-per-second: must be between {REQUESTS_PER_SECOND_MIN} and {REQUESTS_PER_SECOND_MAX} (inclusive)"
            ));
        }
        if !(BURST_MIN..=BURST_MAX).contains(&burst) {
            return Err(format!(
                "machine-identity.burst: must be between {BURST_MIN} and {BURST_MAX} (inclusive)"
            ));
        }
        if !(WAIT_TIMEOUT_SECS_MIN..=WAIT_TIMEOUT_SECS_MAX).contains(&wait_timeout_secs) {
            return Err(format!(
                "machine-identity.wait-timeout-secs: must be between {WAIT_TIMEOUT_SECS_MIN} and {WAIT_TIMEOUT_SECS_MAX} (inclusive)"
            ));
        }
        if !(SIGN_TIMEOUT_SECS_MIN..=SIGN_TIMEOUT_SECS_MAX).contains(&sign_timeout_secs) {
            return Err(format!(
                "machine-identity.sign-timeout-secs: must be between {SIGN_TIMEOUT_SECS_MIN} and {SIGN_TIMEOUT_SECS_MAX} (inclusive)"
            ));
        }

        let sign_proxy_url = sign_proxy_url
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let sign_proxy_tls_root_ca = sign_proxy_tls_root_ca
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        if sign_proxy_url.is_none() && sign_proxy_tls_root_ca.is_some() {
            return Err(
                "machine-identity.sign-proxy-tls-root-ca: requires machine-identity.sign-proxy-url"
                    .to_string(),
            );
        }

        Ok(Self {
            requests_per_second,
            burst,
            wait_timeout_secs,
            sign_timeout_secs,
            sign_proxy_url,
            sign_proxy_tls_root_ca,
        })
    }
}

impl From<MachineIdentityParams> for FmdsMachineIdentityConfig {
    fn from(p: MachineIdentityParams) -> Self {
        Self {
            requests_per_second: u32::from(p.requests_per_second),
            burst: u32::from(p.burst),
            wait_timeout_secs: u32::from(p.wait_timeout_secs),
            sign_timeout_secs: u32::from(p.sign_timeout_secs),
            sign_proxy_url: p.sign_proxy_url,
            sign_proxy_tls_root_ca: p.sign_proxy_tls_root_ca,
        }
    }
}

impl TryFrom<&FmdsMachineIdentityConfig> for MachineIdentityParams {
    type Error = String;

    fn try_from(p: &FmdsMachineIdentityConfig) -> Result<Self, Self::Error> {
        let requests_per_second = u8::try_from(p.requests_per_second).map_err(|_| {
            "machine-identity.requests-per-second: does not fit in u8 (proto field requests_per_second)"
                .to_string()
        })?;
        let burst = u8::try_from(p.burst).map_err(|_| {
            "machine-identity.burst: does not fit in u8 (proto field burst)".to_string()
        })?;
        let wait_timeout_secs = u8::try_from(p.wait_timeout_secs).map_err(|_| {
            "machine-identity.wait-timeout-secs: does not fit in u8 (proto field wait_timeout_secs)"
                .to_string()
        })?;
        let sign_timeout_secs = u8::try_from(p.sign_timeout_secs).map_err(|_| {
            "machine-identity.sign-timeout-secs: does not fit in u8 (proto field sign_timeout_secs)"
                .to_string()
        })?;

        Self::try_from_limits(
            requests_per_second,
            burst,
            wait_timeout_secs,
            sign_timeout_secs,
            p.sign_proxy_url.as_deref(),
            p.sign_proxy_tls_root_ca.as_deref(),
        )
    }
}

/// Rate limiting, timeouts, and optional HTTP sign-proxy client for `GET …/meta-data/identity`.
#[derive(Debug)]
pub struct MachineIdentityServing {
    pub governor: Arc<IdentityRateLimiter>,
    pub wait_timeout: Duration,
    pub forge_call_timeout: Duration,
    pub sign_proxy_base: Option<String>,
    pub sign_proxy_http_client: Option<reqwest::Client>,
}

impl MachineIdentityServing {
    /// Defaults match [`MachineIdentityParams::default`].
    pub fn try_default() -> Result<Self, String> {
        Self::try_from_params(MachineIdentityParams::default())
    }

    /// Builds serving state from parsed [`MachineIdentityParams`] (via [`MachineIdentityParams::try_from_limits`]
    /// or `TryFrom<&FmdsMachineIdentityConfig>`). Input must already be normalized (trimmed option strings).
    pub fn try_from_params(params: MachineIdentityParams) -> Result<Self, String> {
        let rps = NonZeroU32::new(u32::from(params.requests_per_second)).ok_or_else(|| {
            "machine-identity.requests-per-second: expected a positive value (internal error)"
                .to_string()
        })?;
        let burst_nz = NonZeroU32::new(u32::from(params.burst)).ok_or_else(|| {
            "machine-identity.burst: expected a positive value (internal error)".to_string()
        })?;
        let identity_quota = Quota::per_second(rps).allow_burst(burst_nz);

        let sign_proxy_base = params.sign_proxy_url.clone();
        let call_timeout = Duration::from_secs(u64::from(params.sign_timeout_secs));
        let sign_proxy_http_client = if sign_proxy_base.is_some() {
            Some(build_sign_proxy_http_client(
                call_timeout,
                params.sign_proxy_tls_root_ca.as_deref(),
            )?)
        } else {
            None
        };

        Ok(Self {
            governor: Arc::new(RateLimiter::direct(identity_quota)),
            wait_timeout: Duration::from_secs(u64::from(params.wait_timeout_secs)),
            forge_call_timeout: call_timeout,
            sign_proxy_base,
            sign_proxy_http_client,
        })
    }
}

/// `governor::RateLimiter` used for `GET …/meta-data/identity` (carbide-agent + carbide-fmds).
pub type IdentityRateLimiter =
    RateLimiter<NotKeyed, InMemoryState, clock::DefaultClock, NoOpMiddleware>;

/// Wait for an IMDS machine-identity rate-limit permit (bounded by `wait_timeout`).
///
/// Returns [`Err`] with [`tokio::time::error::Elapsed`] when waiting exceeds `wait_timeout`.
pub async fn wait_identity_rate_limit_permit(
    governor: &Arc<IdentityRateLimiter>,
    wait_timeout: Duration,
) -> Result<(), tokio::time::error::Elapsed> {
    let lim = Arc::clone(governor);
    tokio::time::timeout(wait_timeout, lim.until_ready()).await?;
    Ok(())
}

/// Call Carbide `SignMachineIdentity` using [`forge_dpu_agent_utils::utils::create_forge_client`] and
/// `machine_identity.sign_timeout_secs` as the overall deadline.
pub async fn sign_machine_identity_with_forge(
    forge_api: &str,
    forge_client_config: &rpc::forge_tls_client::ForgeClientConfig,
    call_timeout: Duration,
    audiences: Vec<String>,
) -> Result<MachineIdentityResponse, tonic::Status> {
    use rpc::forge::MachineIdentityRequest;

    tokio::time::timeout(call_timeout, async {
        let mut client =
            forge_dpu_agent_utils::utils::create_forge_client(forge_api, forge_client_config)
                .await
                .map_err(|e| tonic::Status::internal(e.to_string()))?;
        client
            .sign_machine_identity(MachineIdentityRequest {
                audience: audiences,
            })
            .await
            .map(|r| r.into_inner())
    })
    .await
    .map_err(|_| {
        tonic::Status::deadline_exceeded(
            "timed out calling forge for machine identity (machine-identity.sign-timeout-secs)",
        )
    })?
}

/// Result of [`MetaDataIdentitySigner::machine_identity_response`] and
/// [`MetaDataIdentitySigner::rate_limited_identity_request`]: HTTP sign-proxy response, or
/// Forge payload to build the JSON/text identity body.
pub enum MetaDataIdentityOutcome {
    /// Response from the HTTP sign-proxy (`sign-proxy-url`).
    HttpProxy(Response),
    /// Successful `SignMachineIdentity` result to encode for the client.
    Forge(MachineIdentityResponse),
}

#[async_trait]
pub trait MetaDataIdentitySigner: Send + Sync {
    /// Acquire a rate-limit permit before [`Self::machine_identity_response`].
    async fn wait_identity_permit(&self) -> Result<(), tonic::Status>;

    /// HTTP sign-proxy (if configured), otherwise Carbide `SignMachineIdentity` with `audiences`.
    ///
    /// Implementations should try the sign-proxy first when enabled, then fall back to Forge.
    /// Hold any `Arc`/lock guard across `.await` so sign-proxy config borrows stay valid (see
    /// [`forward_sign_proxy_if_ready`] / [`forward_sign_proxy_http`]).
    async fn machine_identity_response(
        &self,
        uri: &Uri,
        headers: &HeaderMap,
        audiences: Vec<String>,
    ) -> Result<MetaDataIdentityOutcome, tonic::Status>;

    /// Waits for rate-limit capacity, then runs [`Self::machine_identity_response`].
    ///
    /// Default implementation encodes the required ordering; override only if you preserve the same
    /// policy (permit before proxy or Carbide).
    async fn rate_limited_identity_request(
        &self,
        uri: &Uri,
        headers: &HeaderMap,
    ) -> Result<MetaDataIdentityOutcome, tonic::Status> {
        self.wait_identity_permit().await?;
        let audiences = parse_identity_audiences(uri);
        self.machine_identity_response(uri, headers, audiences)
            .await
    }
}

/// Parses repeated `aud` query parameters (URL-decoded).
pub fn parse_identity_audiences(uri: &Uri) -> Vec<String> {
    let Some(query) = uri.query() else {
        return Vec::new();
    };
    url::form_urlencoded::parse(query.as_bytes())
        .filter(|(k, _)| k == "aud")
        .map(|(_, v)| v.into_owned())
        .collect()
}

pub fn metadata_header_is_true(headers: &HeaderMap) -> bool {
    headers
        .get("metadata")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.eq_ignore_ascii_case("true"))
}

/// Returns true when the request carries `X-Forwarded-For` (any value).
///
/// Per SPIFFE/IMDS SDD, identity requests must not include this header.
pub fn request_has_x_forwarded_for(headers: &HeaderMap) -> bool {
    headers.contains_key("x-forwarded-for")
}

pub fn accept_text_plain(headers: &HeaderMap) -> bool {
    headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| {
            a.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("text/plain"))
        })
}

/// HTTP status for failed machine-identity signing (Carbide / implementation errors on the IMDS path).
///
/// gRPC status codes from Carbide are all map to **503 Service Unavailable**
/// The exception is **429 Too Many Requests** for [`tonic::Code::ResourceExhausted`],
/// used for identity rate-limit wait timeouts in this crate.
///
/// The returned message is safe to expose to IMDS clients; details are logged at `WARN`.
pub fn map_machine_identity_signing_error_to_http(status: &tonic::Status) -> (StatusCode, String) {
    use tonic::Code;
    match status.code() {
        Code::ResourceExhausted => (StatusCode::TOO_MANY_REQUESTS, status.message().to_string()),
        code => {
            tracing::warn!(
                grpc_status_code = ?code,
                error = %status.message(),
                "machine-identity: upstream signing failed"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "machine identity signing is temporarily unavailable\n".to_string(),
            )
        }
    }
}

pub fn build_sign_proxy_http_client(
    timeout: Duration,
    root_ca_pem_path: Option<&str>,
) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder().timeout(timeout);
    if let Some(path) = root_ca_pem_path {
        let pem = std::fs::read(path).map_err(|e| {
            format!("machine-identity.sign-proxy-tls-root-ca: failed to read {path}: {e}")
        })?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|e| {
            format!("machine-identity.sign-proxy-tls-root-ca: invalid PEM in {path}: {e}")
        })?;
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }
    builder
        .build()
        .map_err(|e| format!("machine-identity.sign-proxy-url: failed to build HTTP client ({e})"))
}

pub fn build_sign_proxy_request_url(base_url: &str, query: Option<&str>) -> Result<String, String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("machine-identity.sign-proxy-url: base URL is empty".to_string());
    }
    let q = query
        .filter(|q| !q.is_empty())
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    Ok(format!("{base}/{SIGN_PROXY_UPSTREAM_IMDS_PREFIX}{q}"))
}

struct SignProxyReqwestErrorText {
    response_body: String,
    log_error: String,
}

fn sign_proxy_reqwest_error_text(error: reqwest::Error) -> SignProxyReqwestErrorText {
    // Keep the existing reqwest text in the IMDS response. Remove the URL from
    // `log_error` so configured userinfo and forwarded `aud` values do not reach the log.
    let response_body = error.to_string();
    let log_error = error.without_url().to_string();
    SignProxyReqwestErrorText {
        response_body,
        log_error,
    }
}

fn sign_proxy_log_origin(upstream_url: &str) -> String {
    reqwest::Url::parse(upstream_url)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|_| "<invalid sign proxy URL>".to_string())
}

pub async fn forward_sign_proxy_http(
    client: &reqwest::Client,
    base_url: &str,
    request_uri: &Uri,
    headers: &HeaderMap,
) -> Response {
    let upstream_url = match build_sign_proxy_request_url(base_url, request_uri.query()) {
        Ok(u) => u,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };

    let upstream_origin = sign_proxy_log_origin(&upstream_url);
    tracing::debug!(
        upstream = %upstream_origin,
        "forwarding machine identity request to HTTP sign proxy"
    );

    let mut req = client.get(upstream_url);
    if let Some(v) = headers.get("metadata")
        && let Ok(s) = v.to_str()
    {
        req = req.header("Metadata", s);
    }
    if let Some(v) = headers.get(ACCEPT)
        && let Ok(s) = v.to_str()
    {
        req = req.header(ACCEPT, s);
    }

    let upstream = match req.send().await {
        Ok(r) => r,
        Err(error) => {
            let is_timeout = error.is_timeout();
            let code = if is_timeout {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            let error_text = sign_proxy_reqwest_error_text(error);
            let error = error_text.log_error;
            emit(if is_timeout {
                MachineIdentitySignProxyFailed::RequestTimeout { error }
            } else {
                MachineIdentitySignProxyFailed::RequestTransport { error }
            });
            return (code, error_text.response_body).into_response();
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    let content_type = upstream
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| HeaderValue::from_bytes(v.as_bytes()).ok());

    let body_bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(error) => {
            let error_text = sign_proxy_reqwest_error_text(error);
            emit(MachineIdentitySignProxyFailed::ResponseBody {
                error: error_text.log_error,
            });
            return (StatusCode::BAD_GATEWAY, error_text.response_body).into_response();
        }
    };

    let mut res = Response::builder().status(status);
    if let Some(ct) = content_type {
        res = res.header(CONTENT_TYPE, ct);
    }
    let response = match res.body(axum::body::Body::from(body_bytes)) {
        Ok(response) => response,
        Err(error) => {
            let error = error.to_string();
            emit(MachineIdentitySignProxyFailed::ResponseBuild {
                error: error.clone(),
            });
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("machine-identity.sign-proxy-url: failed to build HTTP response ({error})"),
            )
                .into_response();
        }
    };
    if status.is_server_error() {
        emit(MachineIdentitySignProxyFailed::UpstreamServer {});
    }
    response
}

/// When `sign-proxy-url` is configured (`sign_proxy_base` is [`Some`]), forward the identity request
/// to the HTTP sign proxy. Returns [`None`] when sign-proxy is not in use (caller should use Forge).
///
/// If the base URL is set but `sign_proxy_http_client` is missing, returns [`Some`] with HTTP 500
/// (misconfiguration).
pub async fn forward_sign_proxy_if_ready(
    sign_proxy_base: Option<&str>,
    sign_proxy_http_client: Option<&reqwest::Client>,
    uri: &Uri,
    headers: &HeaderMap,
) -> Option<Response> {
    let base = sign_proxy_base?;
    let Some(client) = sign_proxy_http_client else {
        return Some(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "machine-identity.sign-proxy-url: HTTP client is not configured\n",
            )
                .into_response(),
        );
    };
    Some(forward_sign_proxy_http(client, base, uri, headers).await)
}

#[derive(serde::Serialize)]
struct IdentityTokenJsonBody {
    access_token: String,
    issued_token_type: String,
    token_type: String,
    expires_in: u32,
}

pub async fn serve_meta_data_identity<S: MetaDataIdentitySigner + ?Sized>(
    signer: &S,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if !metadata_header_is_true(&headers) {
        return (
            StatusCode::BAD_REQUEST,
            "Metadata: true header is required for meta-data/identity\n",
        )
            .into_response();
    }

    if request_has_x_forwarded_for(&headers) {
        return (
            StatusCode::BAD_REQUEST,
            "X-Forwarded-For header is not permitted for meta-data/identity\n",
        )
            .into_response();
    }

    match signer.rate_limited_identity_request(&uri, &headers).await {
        Ok(MetaDataIdentityOutcome::HttpProxy(resp)) => resp,
        Ok(MetaDataIdentityOutcome::Forge(resp)) => {
            let body = IdentityTokenJsonBody {
                access_token: resp.access_token,
                issued_token_type: resp.issued_token_type,
                token_type: resp.token_type,
                expires_in: resp.expires_in_sec,
            };
            let json = match serde_json::to_string(&body) {
                Ok(s) => s,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("machine-identity: failed to serialize identity response ({e})"),
                    )
                        .into_response();
                }
            };

            let content_type = if accept_text_plain(&headers) {
                "text/plain; charset=utf-8"
            } else {
                "application/json"
            };
            let mut res = (StatusCode::OK, json).into_response();
            res.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static(content_type),
            );
            res
        }
        Err(e) => {
            let (code, body) = map_machine_identity_signing_error_to_http(&e);
            (code, body).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::routing::get;
    use carbide_instrument::testing::{CapturedLog, MetricsCapture, capture_logs};
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use http_body_util::BodyExt;
    use tokio::io::AsyncWriteExt;

    use super::*;

    const SIGN_PROXY_FAILURE_METRIC: &str = "carbide_machine_identity_sign_proxy_failures_total";

    #[test]
    fn parse_identity_audiences_repeated_and_decoded() {
        let uri: Uri = "http://127.0.0.1/latest/meta-data/identity?aud=spiffe%3A%2F%2Fa&aud=b"
            .parse()
            .unwrap();
        assert_eq!(
            parse_identity_audiences(&uri),
            vec!["spiffe://a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn map_machine_identity_signing_error_resource_exhausted_is_429_with_message() {
        let s = tonic::Status::resource_exhausted("rate limit wait");
        let (code, body) = map_machine_identity_signing_error_to_http(&s);
        assert_eq!(code, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body, "rate limit wait");
    }

    #[test]
    fn map_machine_identity_signing_error_permission_denied_is_503_generic() {
        let s = tonic::Status::permission_denied("internal detail");
        let (code, body) = map_machine_identity_signing_error_to_http(&s);
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body,
            "machine identity signing is temporarily unavailable\n"
        );
    }

    #[test]
    fn metadata_header_is_true_accepts_case_insensitive() {
        let mut h = HeaderMap::new();
        assert!(!metadata_header_is_true(&h));
        h.insert("metadata", HeaderValue::from_static("true"));
        assert!(metadata_header_is_true(&h));
        let mut h2 = HeaderMap::new();
        h2.insert("metadata", HeaderValue::from_static("TRUE"));
        assert!(metadata_header_is_true(&h2));
    }

    #[test]
    fn request_has_x_forwarded_for_cases() {
        let cases: &[(&[(&str, &str)], bool)] = &[
            (&[], false),
            (&[("Metadata", "true")], false),
            (&[("X-Forwarded-For", "1.2.3.4")], true),
            (&[("x-forwarded-for", "1.2.3.4")], true),
            (
                &[("Metadata", "true"), ("X-Forwarded-For", "1.2.3.4")],
                true,
            ),
        ];

        for (header_pairs, want) in cases {
            let mut headers = HeaderMap::new();
            for (name, value) in *header_pairs {
                headers.insert(
                    *name,
                    HeaderValue::from_str(value).expect("valid header value"),
                );
            }
            assert_eq!(
                request_has_x_forwarded_for(&headers),
                *want,
                "header_pairs={header_pairs:?}"
            );
        }
    }

    struct StubIdentitySigner;

    #[async_trait]
    impl MetaDataIdentitySigner for StubIdentitySigner {
        async fn wait_identity_permit(&self) -> Result<(), tonic::Status> {
            Ok(())
        }

        async fn machine_identity_response(
            &self,
            _uri: &Uri,
            _headers: &HeaderMap,
            _audiences: Vec<String>,
        ) -> Result<MetaDataIdentityOutcome, tonic::Status> {
            Ok(MetaDataIdentityOutcome::Forge(MachineIdentityResponse {
                access_token: "stub-token".to_string(),
                issued_token_type: "urn:ietf:params:oauth:token-type:jwt".to_string(),
                token_type: "Bearer".to_string(),
                expires_in_sec: 60,
            }))
        }
    }

    #[tokio::test]
    async fn serve_meta_data_identity_rejects_x_forwarded_for() {
        let uri: Uri = "http://169.254.169.254/latest/meta-data/identity?aud=test"
            .parse()
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("metadata", HeaderValue::from_static("true"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4"));

        let response = serve_meta_data_identity(&StubIdentitySigner, uri, headers).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            &body[..],
            b"X-Forwarded-For header is not permitted for meta-data/identity\n"
        );
    }

    #[tokio::test]
    async fn serve_meta_data_identity_allows_valid_metadata_only_request() {
        let uri: Uri = "http://169.254.169.254/latest/meta-data/identity?aud=test"
            .parse()
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("metadata", HeaderValue::from_static("true"));

        let response = serve_meta_data_identity(&StubIdentitySigner, uri, headers).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn machine_identity_params_try_from_proto_trims_url() {
        let p = FmdsMachineIdentityConfig {
            requests_per_second: 5,
            burst: 10,
            wait_timeout_secs: 3,
            sign_timeout_secs: 6,
            sign_proxy_url: Some("  https://sign.example  ".to_string()),
            sign_proxy_tls_root_ca: None,
        };
        let params = MachineIdentityParams::try_from(&p).unwrap();
        assert_eq!(
            params.sign_proxy_url.as_deref(),
            Some("https://sign.example")
        );
    }

    #[test]
    fn try_from_proto_matches_try_from_limits() {
        let proto = FmdsMachineIdentityConfig {
            requests_per_second: 5,
            burst: 10,
            wait_timeout_secs: 3,
            sign_timeout_secs: 6,
            sign_proxy_url: Some("  https://sign.example  ".to_string()),
            sign_proxy_tls_root_ca: None,
        };
        let a = MachineIdentityParams::try_from(&proto).unwrap();
        let b = MachineIdentityParams::try_from_limits(
            5,
            10,
            3,
            6,
            Some("  https://sign.example  "),
            None,
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(
            FmdsMachineIdentityConfig::from(a),
            FmdsMachineIdentityConfig {
                requests_per_second: 5,
                burst: 10,
                wait_timeout_secs: 3,
                sign_timeout_secs: 6,
                sign_proxy_url: Some("https://sign.example".to_string()),
                sign_proxy_tls_root_ca: None,
            }
        );
    }

    #[test]
    fn accept_text_plain_detects_header() {
        let mut h = HeaderMap::new();
        assert!(!accept_text_plain(&h));
        h.insert(ACCEPT, HeaderValue::from_static("application/json"));
        assert!(!accept_text_plain(&h));
        let mut h2 = HeaderMap::new();
        h2.insert(ACCEPT, HeaderValue::from_static("text/plain"));
        assert!(accept_text_plain(&h2));
    }

    #[test]
    fn build_sign_proxy_request_url_appends_path_and_query() {
        assert_eq!(
            build_sign_proxy_request_url("http://127.0.0.1:9/foo", Some("aud=x")).unwrap(),
            "http://127.0.0.1:9/foo/latest/meta-data/identity?aud=x"
        );
        assert_eq!(
            build_sign_proxy_request_url("http://127.0.0.1:9/foo/", None).unwrap(),
            "http://127.0.0.1:9/foo/latest/meta-data/identity"
        );
    }

    #[derive(Debug, PartialEq)]
    struct SignProxyResponseObservation {
        status: StatusCode,
        content_type: Option<String>,
        body: String,
        counter_deltas: [f64; 5],
    }

    #[tokio::test]
    async fn forward_sign_proxy_http_passes_through_upstream_statuses() {
        let path = format!("/{}", SIGN_PROXY_UPSTREAM_IMDS_PREFIX);
        let app = Router::new().route(
            path.as_str(),
            get(|uri: Uri| async move {
                let status =
                    url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
                        .find_map(|(key, value)| {
                            if key == "status" {
                                value.parse::<u16>().ok()
                            } else {
                                None
                            }
                        })
                        .and_then(|status| StatusCode::from_u16(status).ok())
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                (
                    status,
                    [(CONTENT_TYPE, "application/special")],
                    format!("upstream status {}", status.as_u16()),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let base = format!("http://{}", addr);
        let mut headers = HeaderMap::new();
        headers.insert("metadata", HeaderValue::from_static("true"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        check_cases_async(
            [
                Case {
                    scenario: "successful upstream response",
                    input: StatusCode::CREATED,
                    expect: Yields(SignProxyResponseObservation {
                        status: StatusCode::CREATED,
                        content_type: Some("application/special".to_string()),
                        body: "upstream status 201".to_string(),
                        counter_deltas: [0.0; 5],
                    }),
                },
                Case {
                    scenario: "upstream client rejection",
                    input: StatusCode::BAD_REQUEST,
                    expect: Yields(SignProxyResponseObservation {
                        status: StatusCode::BAD_REQUEST,
                        content_type: Some("application/special".to_string()),
                        body: "upstream status 400".to_string(),
                        counter_deltas: [0.0; 5],
                    }),
                },
                Case {
                    scenario: "upstream service failure",
                    input: StatusCode::SERVICE_UNAVAILABLE,
                    expect: Yields(SignProxyResponseObservation {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        content_type: Some("application/special".to_string()),
                        body: "upstream status 503".to_string(),
                        counter_deltas: [0.0, 0.0, 0.0, 0.0, 1.0],
                    }),
                },
            ],
            |upstream_status| {
                let client = client.clone();
                let base = base.clone();
                let headers = headers.clone();
                async move {
                    let metrics = MetricsCapture::start();
                    let uri: Uri = format!(
                        "http://client/latest/meta-data/identity?status={}",
                        upstream_status.as_u16()
                    )
                    .parse()
                    .unwrap();
                    let response = forward_sign_proxy_http(&client, &base, &uri, &headers).await;
                    let status = response.status();
                    let content_type = response
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string);
                    let body = response.into_body().collect().await.unwrap().to_bytes();

                    Ok::<_, ()>(SignProxyResponseObservation {
                        status,
                        content_type,
                        body: String::from_utf8(body.to_vec()).unwrap(),
                        counter_deltas: [
                            metrics.counter_delta(
                                SIGN_PROXY_FAILURE_METRIC,
                                &[("failure_stage", "request_timeout")],
                            ),
                            metrics.counter_delta(
                                SIGN_PROXY_FAILURE_METRIC,
                                &[("failure_stage", "request_transport")],
                            ),
                            metrics.counter_delta(
                                SIGN_PROXY_FAILURE_METRIC,
                                &[("failure_stage", "response_body")],
                            ),
                            metrics.counter_delta(
                                SIGN_PROXY_FAILURE_METRIC,
                                &[("failure_stage", "response_build")],
                            ),
                            metrics.counter_delta(
                                SIGN_PROXY_FAILURE_METRIC,
                                &[("failure_stage", "upstream_server")],
                            ),
                        ],
                    })
                }
            },
        )
        .await;
        server.abort();
    }

    #[tokio::test]
    async fn sign_proxy_reqwest_error_text_keeps_response_and_strips_log_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let error = client
            .get(format!(
                "http://{addr}/private-signing-path?aud=sensitive-audience"
            ))
            .send()
            .await
            .unwrap_err();
        let error_url = error.url().expect("reqwest error should retain its URL");
        let error_url = error_url.to_string();
        let original_response = error.to_string();
        let error_text = sign_proxy_reqwest_error_text(error);

        assert_eq!(error_text.response_body, original_response);
        assert!(!error_text.log_error.contains(&error_url));
        assert!(!error_text.log_error.contains("sensitive-audience"));
        assert!(!error_text.log_error.contains("private-signing-path"));
    }

    fn assert_redacted_sign_proxy_event(
        logs: &[CapturedLog],
        event_name: &str,
        failure_stage: &str,
        expected_error: &str,
        forbidden_values: &[&str],
    ) {
        let event_logs = logs
            .iter()
            .filter(|log| log.field("event_name") == Some(event_name))
            .collect::<Vec<_>>();
        assert_eq!(event_logs.len(), 1, "one Event owns this failure");
        let log = event_logs[0];
        assert_eq!(log.field("failure_stage"), Some(failure_stage));
        assert_eq!(log.field("error"), Some(expected_error));

        for captured in logs {
            for value in std::iter::once(captured.metadata_name.as_str())
                .chain(std::iter::once(captured.target.as_str()))
                .chain(std::iter::once(captured.message.as_str()))
                .chain(
                    captured
                        .fields
                        .iter()
                        .flat_map(|(name, value)| [name.as_str(), value.as_str()]),
                )
            {
                for forbidden in forbidden_values {
                    assert!(
                        !value.contains(forbidden),
                        "captured log exposed `{forbidden}` in `{value}`",
                    );
                }
            }
        }
    }

    #[test]
    fn forward_sign_proxy_http_counts_and_redacts_transport_failure() {
        const PRIVATE_PATH: &str = "private-signing-root";
        const USERINFO_NAME: &str = "proxy-user";
        const USERINFO_SECRET: &str = "proxy-password-secret";
        const QUERY_SECRET: &str = "transport-audience-secret";

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let base = format!("http://{USERINFO_NAME}:{USERINFO_SECRET}@{addr}/{PRIVATE_PATH}");
        let uri: Uri = format!("http://client/latest/meta-data/identity?aud={QUERY_SECRET}")
            .parse()
            .unwrap();
        let upstream_url = build_sign_proxy_request_url(&base, uri.query()).unwrap();
        let response_url = format!(
            "http://{addr}/{PRIVATE_PATH}/{SIGN_PROXY_UPSTREAM_IMDS_PREFIX}?aud={QUERY_SECRET}"
        );
        let metrics = MetricsCapture::start();
        let mut response = None;
        let logs = capture_logs(|| {
            response = Some(runtime.block_on(forward_sign_proxy_http(
                &client,
                &base,
                &uri,
                &HeaderMap::new(),
            )));
        });
        let response = response.expect("forwarding returns an HTTP response");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = runtime
            .block_on(response.into_body().collect())
            .unwrap()
            .to_bytes();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            format!("error sending request for url ({response_url})"),
            "the IMDS client keeps the historical reqwest error body",
        );
        assert_redacted_sign_proxy_event(
            &logs,
            "machine_identity_sign_proxy_failed",
            "request_transport",
            "error sending request",
            &[
                upstream_url.as_str(),
                PRIVATE_PATH,
                USERINFO_NAME,
                USERINFO_SECRET,
                QUERY_SECRET,
            ],
        );
        assert_eq!(
            metrics.counter_delta(
                SIGN_PROXY_FAILURE_METRIC,
                &[("failure_stage", "request_transport")],
            ),
            1.0
        );
    }

    #[tokio::test]
    async fn forward_sign_proxy_http_counts_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let uri: Uri = "http://client/latest/meta-data/identity".parse().unwrap();
        let metrics = MetricsCapture::start();
        let response =
            forward_sign_proxy_http(&client, &format!("http://{addr}"), &uri, &HeaderMap::new())
                .await;

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            metrics.counter_delta(
                SIGN_PROXY_FAILURE_METRIC,
                &[("failure_stage", "request_timeout")],
            ),
            1.0
        );
        server.abort();
    }

    #[test]
    fn forward_sign_proxy_http_counts_and_redacts_truncated_response_body() {
        const PRIVATE_PATH: &str = "private-response-root";
        const QUERY_SECRET: &str = "response-audience-secret";
        const RESPONSE_SECRET: &str = "partial-response-secret";

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let base = format!("http://{addr}/{PRIVATE_PATH}");
        let uri: Uri = format!("http://client/latest/meta-data/identity?aud={QUERY_SECRET}")
            .parse()
            .unwrap();
        let upstream_url = build_sign_proxy_request_url(&base, uri.query()).unwrap();
        let metrics = MetricsCapture::start();
        let mut response = None;
        let logs = capture_logs(|| {
            response = Some(runtime.block_on(async {
                let server = tokio::spawn(async move {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    socket
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: 64\r\nConnection: close\r\n\r\n{RESPONSE_SECRET}"
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                });
                let response =
                    forward_sign_proxy_http(&client, &base, &uri, &HeaderMap::new()).await;
                server.await.unwrap();
                response
            }));
        });
        let response = response.expect("forwarding returns an HTTP response");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = runtime
            .block_on(response.into_body().collect())
            .unwrap()
            .to_bytes();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            format!("error decoding response body for url ({upstream_url})"),
            "the IMDS client keeps the historical reqwest error body",
        );
        assert_redacted_sign_proxy_event(
            &logs,
            "machine_identity_sign_proxy_failed",
            "response_body",
            "error decoding response body",
            &[
                upstream_url.as_str(),
                PRIVATE_PATH,
                QUERY_SECRET,
                RESPONSE_SECRET,
            ],
        );
        assert_eq!(
            metrics.counter_delta(
                SIGN_PROXY_FAILURE_METRIC,
                &[("failure_stage", "response_body")],
            ),
            1.0
        );
    }

    #[tokio::test]
    async fn forward_sign_proxy_if_ready_returns_none_without_base_url() {
        let uri: Uri = "http://127.0.0.1/latest/meta-data/identity"
            .parse()
            .unwrap();
        let headers = HeaderMap::new();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();

        assert!(
            forward_sign_proxy_if_ready(None, Some(&client), &uri, &headers)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn forward_sign_proxy_if_ready_returns_500_when_http_client_missing() {
        let uri: Uri = "http://127.0.0.1/latest/meta-data/identity"
            .parse()
            .unwrap();
        let headers = HeaderMap::new();

        let res = forward_sign_proxy_if_ready(Some("http://127.0.0.1:1"), None, &uri, &headers)
            .await
            .expect("expected misconfiguration response");
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn forward_sign_proxy_if_ready_delegates_to_forward_http() {
        let path = format!("/{}", SIGN_PROXY_UPSTREAM_IMDS_PREFIX);
        let app = Router::new().route(
            path.as_str(),
            get(|| async {
                (
                    StatusCode::CREATED,
                    [(CONTENT_TYPE, "application/special")],
                    "custom-token-body",
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        let base = format!("http://{}", addr);
        let uri: Uri = "http://client/latest/meta-data/identity?aud=test"
            .parse()
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("metadata", HeaderValue::from_static("true"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        let res = forward_sign_proxy_if_ready(Some(base.as_str()), Some(&client), &uri, &headers)
            .await
            .expect("expected forward response");
        assert_eq!(res.status(), StatusCode::CREATED);
        assert_eq!(
            res.headers().get(CONTENT_TYPE).unwrap().as_bytes(),
            b"application/special"
        );
        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"custom-token-body");
        server.abort();
    }
}
