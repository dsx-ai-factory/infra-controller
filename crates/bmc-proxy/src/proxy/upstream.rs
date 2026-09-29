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

//! The request the proxy sends to a BMC: its address, the caller's headers
//! and body, the credential for the attempt, and the HTTP client it goes out
//! on.

use std::borrow::Cow;
use std::net::{IpAddr, Ipv6Addr};
use std::time::{Duration, Instant};

use axum::body::Body;
use carbide_instrument::emit;
use carbide_utils::HostPortPair;
use http::{HeaderMap, Method, Response, StatusCode, Uri};
use rpc::forge_api_client::ForgeApiClient;
use trace_propagation::is_propagated_header;

use crate::class::DEFAULT_UPSTREAM_TIMEOUT;
use crate::metrics::{MethodLabel, UpstreamRequestCompleted, UpstreamStatus};
use crate::proxy::credentials::{
    BmcCredentials, CredentialCache, REDFISH_AUTH_TOKEN_HEADER, get_bmc_credentials,
};
use crate::proxy::{BmcProxyError, BmcProxyState, ProxyError, error_response};
use crate::span_isolation::SpanIsolationMiddleware;

/// Request bodies up to this size are buffered and forwarded with the exact framing
/// BMCs have always seen. Anything larger -- Redfish multipart firmware
/// pushes, mainly -- is streamed through instead of being rejected, which the
/// old 8 MiB hard cap did. (8 MiB matches nginx ingress controller defaults.)
pub(super) const MAX_BUFFERED_BODY_SIZE: usize = 8 * 1024 * 1024;

/// Floor transfer rate used to scale a streamed upload's timeout from its
/// declared size, mirroring libredfish's firmware-upload heuristic. A class's
/// budget, at most 30 minutes, would abort a large image mid-push.
const MIN_UPLOAD_BANDWIDTH_BYTES_PER_SEC: u64 = 10_000;

/// Ceiling on a streamed upload's scaled timeout. The declared length is
/// caller-supplied, so without a cap a lying `Content-Length` would let a
/// stalled request pin a proxy task and a BMC connection indefinitely. Four
/// hours covers any real firmware image at the floor rate.
const MAX_UPLOAD_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);

/// The caller's request body, in a form the proxy can attach to an upstream
/// request -- and, when buffered, attach again for one retry.
pub(super) enum UpstreamBody {
    None,
    /// Small or unsized bodies, buffered up to [`MAX_BUFFERED_BODY_SIZE`].
    Buffered(bytes::Bytes),
    /// A large body with a declared length is streamed straight through and
    /// can be sent only once; `None` once consumed.
    Streamed {
        body: Option<Body>,
        declared_length: u64,
    },
}

/// The final response and the authentication secrets used for that exact
/// attempt. Keeping them together ensures a refreshed credential is used to
/// sanitize the response produced by the retry, not the stale first attempt.
pub(super) struct UpstreamResponse {
    pub(super) response: reqwest::Response,
    pub(super) sensitive_values: Vec<String>,
}

impl UpstreamBody {
    pub(super) async fn prepare(headers: &HeaderMap, body: Body) -> Result<Self, axum::Error> {
        let declared_length = headers
            .get(axum::http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        match declared_length {
            Some(length) if length > MAX_BUFFERED_BODY_SIZE as u64 => Ok(Self::Streamed {
                body: Some(body),
                declared_length: length,
            }),
            _ => Ok(Self::Buffered(
                axum::body::to_bytes(body, MAX_BUFFERED_BODY_SIZE).await?,
            )),
        }
    }

    pub(super) fn is_replayable(&self) -> bool {
        matches!(self, Self::None | Self::Buffered(_))
    }

    /// Attaches the body to `request` with the exchange's budget: `timeout`
    /// for a body sent whole, or one scaled to a streamed body's declared
    /// size. A streamed body is handed over on the first call; a later call
    /// finds it consumed and fails.
    fn attach(
        &mut self,
        request: reqwest_middleware::RequestBuilder,
        timeout: Duration,
    ) -> Result<reqwest_middleware::RequestBuilder, ProxyError> {
        match self {
            Self::None => Ok(request.timeout(timeout)),
            Self::Buffered(bytes) => Ok(request.timeout(timeout).body(bytes.clone())),
            Self::Streamed {
                body,
                declared_length,
            } => {
                // A streamed body cannot be replayed, so reqwest's redirect
                // layer forwards a BMC 307/308 to the caller as-is instead of
                // following it the way buffered requests do. Callers pushing
                // firmware should use the canonical UpdateService URI rather
                // than rely on redirects.
                let body = body.take().ok_or_else(|| {
                    ProxyError::from((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "streamed request body already consumed".to_string(),
                    ))
                })?;
                Ok(request
                    .header(axum::http::header::CONTENT_LENGTH, *declared_length)
                    .timeout(sized_upload_timeout(*declared_length))
                    .body(reqwest::Body::wrap_stream(body.into_data_stream())))
            }
        }
    }
}

/// One forwarding attempt: resolve credentials for `target_ip` (cached or
/// freshly minted), build the upstream request from the caller's `parts`,
/// attach the body with the exchange's budget (see [`UpstreamBody::attach`]),
/// and send. Records the per-attempt upstream metric.
pub(super) async fn send_upstream(
    state: &BmcProxyState,
    target_ip: IpAddr,
    parts: &http::request::Parts,
    path_and_query: http::uri::PathAndQuery,
    upstream_body: &mut UpstreamBody,
    timeout: Duration,
) -> Result<UpstreamResponse, Response<Body>> {
    let mut bmc_client_info = create_client(
        target_ip,
        &state.api_client,
        &state.credential_cache,
        state.http_client.clone(),
        &state.config.bmc_proxy,
    )
    .await
    .map_err(|e| error_response((StatusCode::BAD_GATEWAY, e.to_string()).into()))?;

    copy_request_headers(&parts.headers, &mut bmc_client_info.header_map);

    let mut upstream_uri_parts = bmc_client_info.base_upstream_uri.into_parts();
    upstream_uri_parts.path_and_query = Some(path_and_query);
    let upstream_uri = Uri::from_parts(upstream_uri_parts)
        .map_err(|e| error_response((StatusCode::BAD_REQUEST, e.to_string()).into()))?;

    let upstream_request = bmc_client_info
        .http_client
        .request(parts.method.clone(), upstream_uri.to_string())
        .headers(bmc_client_info.header_map);
    // Apply the credential and retain every secret representation placed on
    // the wire. The downstream client cannot reconstruct this context safely.
    let (upstream_request, sensitive_values) = bmc_client_info
        .credentials
        .apply_to_request(upstream_request)
        .map_err(|e| {
            error_response((StatusCode::BAD_GATEWAY, format!("invalid credentials: {e}")).into())
        })?;
    let upstream_request = upstream_body
        .attach(upstream_request, timeout)
        .map_err(error_response)?;

    let started = Instant::now();
    let upstream_result = upstream_request.send().await;
    emit(UpstreamRequestCompleted {
        method: MethodLabel::from(&parts.method),
        status: UpstreamStatus::from_result(&upstream_result),
        took: started.elapsed(),
    });
    upstream_result
        .map(|response| UpstreamResponse {
            response,
            sensitive_values,
        })
        .map_err(|e| error_response((StatusCode::BAD_GATEWAY, e.to_string()).into()))
}

fn copy_request_headers(source: &HeaderMap, dest: &mut HeaderMap) {
    for (name, value) in source {
        if is_hop_by_hop_header(name.as_str())
            // Trace context describes the caller's hop; the upstream client's tracing middleware
            // re-injects the proxy's own hop on egress.
            || is_propagated_header(name.as_str())
            || *name == axum::http::header::HOST
            || *name == axum::http::header::AUTHORIZATION
            || name.as_str().eq_ignore_ascii_case(REDFISH_AUTH_TOKEN_HEADER)
            || name.as_str().eq_ignore_ascii_case("forwarded")
            || *name == axum::http::header::CONTENT_LENGTH
        {
            continue;
        }
        dest.append(name.clone(), value.clone());
    }
}

pub(super) fn method_supports_body(method: &Method) -> bool {
    // Redfish services can accept DELETE payloads, so only the methods this
    // proxy treats as bodyless are excluded.
    !matches!(*method, Method::GET | Method::HEAD)
}

/// Time allowed for a streamed upload of `length` bytes, whatever its class:
/// [`DEFAULT_UPSTREAM_TIMEOUT`] plus the transfer itself at worst-case OOB
/// bandwidth,
/// bounded by [`MAX_UPLOAD_TIMEOUT`] because `length` is caller-supplied.
fn sized_upload_timeout(length: u64) -> Duration {
    (DEFAULT_UPSTREAM_TIMEOUT + Duration::from_secs(length / MIN_UPLOAD_BANDWIDTH_BYTES_PER_SEC))
        .min(MAX_UPLOAD_TIMEOUT)
}

pub(super) fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

struct BmcClientInfo {
    http_client: reqwest_middleware::ClientWithMiddleware,
    header_map: HeaderMap,
    credentials: BmcCredentials,
    base_upstream_uri: Uri,
}

/// Format a host as a URI authority component, bracketing bare IPv6 literals
/// and appending the port when present.
///
/// A bare IPv6 address such as `2001:db8::1` is not a valid URI authority — it
/// must be bracketed (`[2001:db8::1]`). Without brackets, `http::uri::Authority`
/// parsing (used by the caller to build the upstream URI) rejects the host, and
/// an appended port is misparsed as part of the address.
///
/// The parse guard here covers operator-supplied override hosts, which are
/// genuinely strings; the BMC's own typed `IpAddr` is bracketed off its enum
/// variant by the caller and passes through unchanged (as do IPv4 addresses
/// and hostnames).
fn build_authority(host: Cow<'_, str>, port: Option<u16>) -> Cow<'_, str> {
    let host = if host.parse::<Ipv6Addr>().is_ok() {
        Cow::Owned(format!("[{host}]"))
    } else {
        host
    };
    match port {
        Some(port) => Cow::Owned(format!("{host}:{port}")),
        None => host,
    }
}

async fn create_client(
    ip: IpAddr,
    api_client: &ForgeApiClient,
    credential_cache: &CredentialCache,
    http_client: reqwest_middleware::ClientWithMiddleware,
    bmc_proxy: &Option<HostPortPair>,
) -> Result<BmcClientInfo, BmcProxyError> {
    // Bracket the BMC's own IP off its typed variant (IPv4 renders unchanged),
    // mirroring health::BmcAddr::to_url() and the nv-redfish client.
    let bmc_host = match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    };
    let (host, port, add_custom_header) = match bmc_proxy {
        // No override
        None => (Cow::<str>::Owned(bmc_host), None, false),
        // Override the host and port
        Some(HostPortPair::HostAndPort(h, p)) => (Cow::Borrowed(h.as_str()), Some(*p), true),
        // Only override the host
        Some(HostPortPair::HostOnly(h)) => (Cow::Borrowed(h.as_str()), None, true),
        // Only override the port
        Some(HostPortPair::PortOnly(p)) => (Cow::Owned(bmc_host), Some(*p), false),
    };
    let mut header_map = HeaderMap::new();
    if add_custom_header {
        header_map.insert("forwarded", format!("host={ip}").parse().unwrap());
    }
    let credentials = get_bmc_credentials(ip, api_client, credential_cache).await?;

    let base_authority = build_authority(host, port);

    let base_upstream_uri = Uri::builder()
        .scheme("https")
        .authority(base_authority.as_ref())
        .path_and_query("/")
        .build()
        .map_err(|e| {
            BmcProxyError::InternalProxying(format!("Error building upstream URI: {e}"))
        })?;

    Ok(BmcClientInfo {
        http_client,
        header_map,
        credentials,
        base_upstream_uri,
    })
}

pub(super) fn build_http_client() -> Result<reqwest_middleware::ClientWithMiddleware, BmcProxyError>
{
    let client = reqwest::Client::builder()
        // Keep the proxy's error-sanitization boundary explicit even if a
        // workspace dependency enables a reqwest decompression feature later.
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(std::time::Duration::from_secs(5)) // Limit connections to 5 seconds
        // A backstop: every request sets its own budget when its body is attached.
        .timeout(DEFAULT_UPSTREAM_TIMEOUT)
        .pool_max_idle_per_host(4)
        .build()
        .map_err(|err| {
            tracing::error!(error = %err, "build_http_client");
            BmcProxyError::InternalProxying(format!("Http building failed: {err}"))
        })?;
    Ok(reqwest_middleware::ClientBuilder::new(client)
        .with(reqwest_tracing::TracingMiddleware::default())
        .with(SpanIsolationMiddleware)
        .build())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, Check, check_cases_async, check_values, value_scenarios};
    use carbide_utils::HostPortPair;
    use rpc::forge_api_client::ForgeApiClient;
    use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};

    use super::{
        MAX_BUFFERED_BODY_SIZE, UpstreamBody, build_authority, build_http_client,
        copy_request_headers, create_client, is_hop_by_hop_header, method_supports_body,
    };
    use crate::proxy::credentials::{BmcCredentials, CREDENTIAL_CACHE_IDLE_TTL, CredentialCache};
    use crate::proxy::idle_bounded_cache;
    use crate::proxy::test_support::*;

    #[derive(Clone, Copy)]
    enum HeaderCopyCase {
        ContentType,
        Custom,
        Host,
        Authorization,
        AuthToken,
        Forwarded,
        AcceptEncoding,
        ContentLength,
        Connection,
        Upgrade,
        TraceParent,
        TraceState,
    }

    #[derive(Clone, Copy)]
    enum ProxyOverrideCase {
        Direct,
        HostOnly,
        PortOnly,
        HostAndPort,
    }

    #[derive(Debug, PartialEq)]
    struct ClientSummary {
        base_upstream_uri: String,
        forwarded_header: Option<String>,
        credentials: CredentialSummary,
    }

    fn header_for_copy_case(case: HeaderCopyCase) -> (HeaderName, HeaderValue) {
        match case {
            HeaderCopyCase::ContentType => (
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            HeaderCopyCase::Custom => (
                HeaderName::from_static("x-request-id"),
                HeaderValue::from_static("request-1"),
            ),
            HeaderCopyCase::Host => (
                axum::http::header::HOST,
                HeaderValue::from_static("bmc.example.com"),
            ),
            HeaderCopyCase::Authorization => (
                axum::http::header::AUTHORIZATION,
                HeaderValue::from_static("Bearer secret"),
            ),
            // One spelling suffices: `HeaderName` lowercases on construction,
            // so a caller's "X-Auth-Token" and "x-auth-token" are the same
            // key by the time the filter sees them.
            HeaderCopyCase::AuthToken => (
                HeaderName::from_static("x-auth-token"),
                HeaderValue::from_static("caller-session"),
            ),
            HeaderCopyCase::Forwarded => (
                HeaderName::from_static("forwarded"),
                HeaderValue::from_static("host=10.0.0.1"),
            ),
            HeaderCopyCase::AcceptEncoding => (
                axum::http::header::ACCEPT_ENCODING,
                HeaderValue::from_static("gzip, br"),
            ),
            HeaderCopyCase::ContentLength => (
                axum::http::header::CONTENT_LENGTH,
                HeaderValue::from_static("42"),
            ),
            HeaderCopyCase::Connection => (
                axum::http::header::CONNECTION,
                HeaderValue::from_static("keep-alive"),
            ),
            HeaderCopyCase::Upgrade => (
                axum::http::header::UPGRADE,
                HeaderValue::from_static("websocket"),
            ),
            HeaderCopyCase::TraceParent => (
                HeaderName::from_static("traceparent"),
                HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
            ),
            HeaderCopyCase::TraceState => (
                HeaderName::from_static("tracestate"),
                HeaderValue::from_static("vendor=value"),
            ),
        }
    }

    fn copied_header_names(case: HeaderCopyCase) -> Vec<String> {
        // Trace-header filtering asks the global propagator which headers are its own, so the
        // propagator `setup_logging` installs at startup has to be in place for the trace cases to
        // mean anything. Installing it here rather than relying on another test having run keeps
        // this independent of test ordering.
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );

        let (name, value) = header_for_copy_case(case);
        let mut source = HeaderMap::new();
        source.insert(name, value);
        let mut dest = HeaderMap::new();

        copy_request_headers(&source, &mut dest);

        dest.keys().map(|name| name.to_string()).collect()
    }

    fn proxy_override(case: ProxyOverrideCase) -> Option<HostPortPair> {
        match case {
            ProxyOverrideCase::Direct => None,
            ProxyOverrideCase::HostOnly => Some(HostPortPair::HostOnly("proxy.local".to_string())),
            ProxyOverrideCase::PortOnly => Some(HostPortPair::PortOnly(8443)),
            ProxyOverrideCase::HostAndPort => {
                Some(HostPortPair::HostAndPort("proxy.local".to_string(), 8443))
            }
        }
    }

    async fn summarize_created_client(case: ProxyOverrideCase) -> Result<ClientSummary, String> {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        // Prepopulate the cache so this test never falls through to the real
        // ForgeApiClient path.
        let credential_cache: CredentialCache = idle_bounded_cache(CREDENTIAL_CACHE_IDLE_TTL);
        credential_cache
            .insert(
                ip,
                BmcCredentials::UsernamePassword {
                    username: "admin".to_string(),
                    password: "secret".to_string(),
                },
            )
            .await;
        let client_config = ForgeClientConfig::default();
        let api_config = ApiConfig::new("https://example.com", &client_config);
        let api_client = ForgeApiClient::new(&api_config);

        create_client(
            ip,
            &api_client,
            &credential_cache,
            build_http_client().expect("test HTTP client builds"),
            &proxy_override(case),
        )
        .await
        .map(|client| ClientSummary {
            base_upstream_uri: client.base_upstream_uri.to_string(),
            forwarded_header: client.header_map.get("forwarded").map(|value| {
                value
                    .to_str()
                    .expect("forwarded header is UTF-8")
                    .to_string()
            }),
            credentials: summarize_credentials(client.credentials),
        })
        .map_err(|error| error.to_string())
    }

    #[test]
    fn build_authority_brackets_ipv6() {
        value_scenarios!(
            run = |(host, port): (&str, Option<u16>)| {
                let authority = build_authority(Cow::Borrowed(host), port).into_owned();
                // The result is fed into `Uri::builder().authority(..)`, which
                // rejects a bare IPv6 literal — guard that it always parses.
                assert!(
                    authority.parse::<http::uri::Authority>().is_ok(),
                    "produced an invalid authority: {authority}"
                );
                authority
            };
            "IPv4 without port" {
                ("192.0.2.5", None) => "192.0.2.5".to_string(),
            }

            "IPv4 with port" {
                ("192.0.2.5", Some(443)) => "192.0.2.5:443".to_string(),
            }

            "bare IPv6 is bracketed" {
                ("2001:db8::1", None) => "[2001:db8::1]".to_string(),
            }

            "bare IPv6 with port is bracketed" {
                ("2001:db8::1", Some(443)) => "[2001:db8::1]:443".to_string(),
            }

            "already bracketed IPv6 is left unchanged" {
                ("[2001:db8::1]", Some(443)) => "[2001:db8::1]:443".to_string(),
            }

            "hostname is left unchanged" {
                ("bmc.example.com", Some(443)) => "bmc.example.com:443".to_string(),
            }
        );
    }

    #[test]
    fn body_method_support() {
        value_scenarios!(
            run = |method| method_supports_body(&method);
            "GET has no upstream body" {
                Method::GET => false,
            }

            "HEAD has no upstream body" {
                Method::HEAD => false,
            }

            "POST supports body" {
                Method::POST => true,
            }

            "PUT supports body" {
                Method::PUT => true,
            }

            "PATCH supports body" {
                Method::PATCH => true,
            }

            "DELETE supports body for Redfish compatibility" {
                Method::DELETE => true,
            }
        );
    }

    #[test]
    fn hop_by_hop_header_detection() {
        value_scenarios!(
            run = is_hop_by_hop_header;
            "connection" {
                "connection" => true,
            }

            "case-insensitive keep-alive" {
                "Keep-Alive" => true,
            }

            "proxy authenticate" {
                "proxy-authenticate" => true,
            }

            "proxy authorization" {
                "proxy-authorization" => true,
            }

            "te" {
                "te" => true,
            }

            "trailer" {
                "trailer" => true,
            }

            "transfer encoding" {
                "transfer-encoding" => true,
            }

            "upgrade" {
                "upgrade" => true,
            }

            "content type is safe" {
                "content-type" => false,
            }
        );
    }

    #[test]
    fn request_header_copying_filters_proxy_owned_headers() {
        value_scenarios!(
            run = copied_header_names;
            "content type copied" {
                HeaderCopyCase::ContentType => vec!["content-type".to_string()],
            }

            "custom header copied" {
                HeaderCopyCase::Custom => vec!["x-request-id".to_string()],
            }

            "host filtered" {
                HeaderCopyCase::Host => vec![],
            }

            "authorization filtered" {
                HeaderCopyCase::Authorization => vec![],
            }

            // The proxy authenticates upstream itself; forwarding a caller's
            // own session token would reach the BMC alongside ours.
            "redfish auth token filtered" {
                HeaderCopyCase::AuthToken => vec![],
            }

            "forwarded filtered" {
                HeaderCopyCase::Forwarded => vec![],
            }

            "accept encoding copied" {
                HeaderCopyCase::AcceptEncoding => vec!["accept-encoding".to_string()],
            }

            "content length filtered" {
                HeaderCopyCase::ContentLength => vec![],
            }

            "connection filtered" {
                HeaderCopyCase::Connection => vec![],
            }

            "upgrade filtered" {
                HeaderCopyCase::Upgrade => vec![],
            }

            "traceparent filtered" {
                HeaderCopyCase::TraceParent => vec![],
            }

            "tracestate filtered" {
                HeaderCopyCase::TraceState => vec![],
            }
        );
    }

    #[test]
    fn upstream_requests_preserve_the_callers_response_encoding_preference() {
        let mut source = HeaderMap::new();
        source.insert(
            axum::http::header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, identity;q=0"),
        );
        let mut dest = HeaderMap::new();

        copy_request_headers(&source, &mut dest);

        assert_eq!(
            dest.get(axum::http::header::ACCEPT_ENCODING),
            Some(&HeaderValue::from_static("gzip, identity;q=0"))
        );
    }

    #[tokio::test]
    async fn client_creation_applies_proxy_overrides() {
        check_cases_async(
            [
                Case {
                    scenario: "direct BMC IP",
                    input: ProxyOverrideCase::Direct,
                    expect: Yields(ClientSummary {
                        base_upstream_uri: "https://10.0.0.5/".to_string(),
                        forwarded_header: None,
                        credentials: CredentialSummary::UsernamePassword {
                            username: "admin".to_string(),
                            password: "secret".to_string(),
                        },
                    }),
                },
                Case {
                    scenario: "proxy host only",
                    input: ProxyOverrideCase::HostOnly,
                    expect: Yields(ClientSummary {
                        base_upstream_uri: "https://proxy.local/".to_string(),
                        forwarded_header: Some("host=10.0.0.5".to_string()),
                        credentials: CredentialSummary::UsernamePassword {
                            username: "admin".to_string(),
                            password: "secret".to_string(),
                        },
                    }),
                },
                Case {
                    scenario: "proxy port only",
                    input: ProxyOverrideCase::PortOnly,
                    expect: Yields(ClientSummary {
                        base_upstream_uri: "https://10.0.0.5:8443/".to_string(),
                        forwarded_header: None,
                        credentials: CredentialSummary::UsernamePassword {
                            username: "admin".to_string(),
                            password: "secret".to_string(),
                        },
                    }),
                },
                Case {
                    scenario: "proxy host and port",
                    input: ProxyOverrideCase::HostAndPort,
                    expect: Yields(ClientSummary {
                        base_upstream_uri: "https://proxy.local:8443/".to_string(),
                        forwarded_header: Some("host=10.0.0.5".to_string()),
                        credentials: CredentialSummary::UsernamePassword {
                            username: "admin".to_string(),
                            password: "secret".to_string(),
                        },
                    }),
                },
            ],
            summarize_created_client,
        )
        .await;
    }

    /// One observation of [`UpstreamBody::attach`]: whether the built request
    /// carries a buffered body (`as_bytes()` is `Some`), an explicit
    /// `Content-Length`, and a per-request timeout override.
    #[derive(Debug, PartialEq)]
    struct AttachedBodySummary {
        buffered: bool,
        explicit_content_length: Option<String>,
        timeout_secs: Option<u64>,
    }

    /// The budget of the class the test requests belong to.
    const CLASS_BUDGET: Duration = Duration::from_secs(45);

    async fn observe_attached_body(
        declared_length: Option<u64>,
    ) -> Result<AttachedBodySummary, String> {
        let client = build_http_client().map_err(|e| e.to_string())?;
        let mut headers = HeaderMap::new();
        if let Some(length) = declared_length {
            headers.insert(
                axum::http::header::CONTENT_LENGTH,
                HeaderValue::from_str(&length.to_string()).expect("valid header value"),
            );
        }

        let request = UpstreamBody::prepare(&headers, Body::from("payload"))
            .await
            .map_err(|e| e.to_string())?
            .attach(
                client.post("https://bmc.invalid/redfish/v1/UpdateService"),
                CLASS_BUDGET,
            )
            .map_err(|e| e.message)?
            .build()
            .map_err(|e| e.to_string())?;

        Ok(AttachedBodySummary {
            buffered: request.body().is_some_and(|b| b.as_bytes().is_some()),
            explicit_content_length: request
                .headers()
                .get(axum::http::header::CONTENT_LENGTH)
                .map(|v| v.to_str().expect("UTF-8 header").to_string()),
            timeout_secs: request.timeout().map(Duration::as_secs),
        })
    }

    // The framing contract: small and unsized bodies are buffered so BMCs see
    // exactly what they always did; only a body declared larger than the
    // buffer bound streams, with its length forwarded (hyper would otherwise
    // switch to chunked transfer, which BMC firmwares commonly reject) and a
    // timeout scaled to the transfer instead of its class's budget.
    #[tokio::test]
    async fn request_bodies_buffer_small_and_stream_large() {
        let large = (MAX_BUFFERED_BODY_SIZE as u64) + 1;
        check_cases_async(
            [
                Case {
                    scenario: "no declared length stays buffered",
                    input: None,
                    expect: Yields(AttachedBodySummary {
                        buffered: true,
                        explicit_content_length: None,
                        timeout_secs: Some(CLASS_BUDGET.as_secs()),
                    }),
                },
                Case {
                    scenario: "small declared length stays buffered",
                    input: Some(1024),
                    expect: Yields(AttachedBodySummary {
                        buffered: true,
                        explicit_content_length: None,
                        timeout_secs: Some(CLASS_BUDGET.as_secs()),
                    }),
                },
                Case {
                    scenario: "length at the bound stays buffered",
                    input: Some(MAX_BUFFERED_BODY_SIZE as u64),
                    expect: Yields(AttachedBodySummary {
                        buffered: true,
                        explicit_content_length: None,
                        timeout_secs: Some(CLASS_BUDGET.as_secs()),
                    }),
                },
                Case {
                    scenario: "length past the bound streams",
                    input: Some(large),
                    expect: Yields(AttachedBodySummary {
                        buffered: false,
                        explicit_content_length: Some(large.to_string()),
                        timeout_secs: Some(super::sized_upload_timeout(large).as_secs()),
                    }),
                },
            ],
            observe_attached_body,
        )
        .await;
    }

    #[test]
    fn sized_upload_timeout_scales_with_declared_length() {
        check_values(
            [
                Check {
                    scenario: "tiny transfer keeps the base budget",
                    input: 1024,
                    expect: 60,
                },
                Check {
                    scenario: "a 50 MB image gets its transfer time",
                    input: 50_000_000,
                    expect: 60 + 5_000,
                },
                // The declared length is caller-supplied; a lie must not buy
                // an unbounded hold on a proxy task and a BMC connection.
                Check {
                    scenario: "a lying length is capped",
                    input: u64::MAX,
                    expect: 4 * 60 * 60,
                },
            ],
            |length| super::sized_upload_timeout(length).as_secs(),
        );
    }

    // The retry contract: a buffered body can be attached again for the one
    // credential-refresh replay, a streamed body cannot (its bytes were
    // consumed by the first attempt), and a bodyless request is trivially
    // replayable.
    #[tokio::test]
    async fn only_buffered_or_absent_bodies_are_replayable() {
        let client = build_http_client().expect("http client");
        let post = || client.post("https://bmc.invalid/redfish/v1/Systems");

        let mut none = UpstreamBody::None;
        assert!(none.is_replayable());
        let _ = none.attach(post(), CLASS_BUDGET).expect("first attach");
        let _ = none
            .attach(post(), CLASS_BUDGET)
            .expect("bodyless requests replay freely");

        let mut buffered = UpstreamBody::prepare(&HeaderMap::new(), Body::from("payload"))
            .await
            .expect("small bodies buffer");
        assert!(buffered.is_replayable());
        let _ = buffered.attach(post(), CLASS_BUDGET).expect("first attach");
        let _ = buffered
            .attach(post(), CLASS_BUDGET)
            .expect("a buffered body replays for the credential-refresh retry");

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_LENGTH,
            HeaderValue::from_str(&(MAX_BUFFERED_BODY_SIZE as u64 + 1).to_string()).unwrap(),
        );
        let mut streamed = UpstreamBody::prepare(&headers, Body::from("payload"))
            .await
            .expect("large declared bodies stream");
        assert!(
            !streamed.is_replayable(),
            "a streamed body must never be replayed"
        );
        let _ = streamed
            .attach(post(), CLASS_BUDGET)
            .expect("first attach consumes the stream");
        let Err(err) = streamed.attach(post(), CLASS_BUDGET) else {
            panic!("a consumed stream cannot be attached again");
        };
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
