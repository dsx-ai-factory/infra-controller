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

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{CONNECTION, FORWARDED, HOST};
use axum::http::uri::Authority;
use axum::http::{Request, StatusCode, Version};
use axum::response::Response;
use axum::routing::any;
use tokio::sync::RwLock;

use crate::router::call_router_with_new_request;

/// Multiplexes Axum routers on a single listener using request authority.
///
/// A `Forwarded` host is authoritative. When present, only that authority is
/// tried. Otherwise the `Host` header and URI authority are tried in that order.
/// Each authority is tried exactly, then without its port or IPv6 brackets.
/// The router stored under an empty string is the final fallback in either case.
/// Missing authorities return 404 and retire HTTP/1 connections so a subsequent
/// connection can follow an endpoint whose address was reassigned.
pub fn authority_router(routers: Arc<RwLock<HashMap<String, Router>>>) -> Router {
    Router::new()
        .route("/{*all}", any(process))
        .with_state(AuthorityRouter { routers })
}

#[derive(Clone)]
struct AuthorityRouter {
    routers: Arc<RwLock<HashMap<String, Router>>>,
}

/// The authority a client addressed this request to, in the same precedence
/// [`authority_router`] uses: the `host` parameter of `Forwarded`, the `Host`
/// header, then the URI authority (where HTTP/2 carries `:authority`).
pub fn request_authority<B>(request: &Request<B>) -> Option<String> {
    forwarded_host(request)
        .or_else(|| host_header(request))
        .or_else(|| request.uri().authority().map(|v| v.as_str().to_owned()))
}

/// The `Host` header, when present and valid UTF-8.
fn host_header<B>(request: &Request<B>) -> Option<String> {
    request
        .headers()
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned)
}

/// Dispatches using the authoritative forwarded host, or the transport's `Host`
/// header and URI authority when no forwarded host is present.
async fn process(State(state): State<AuthorityRouter>, request: Request<Body>) -> Response {
    let forwarded_host = forwarded_host(&request);
    let host = host_header(&request);
    let authority = request.uri().authority().map(|v| v.as_str().to_owned());
    let router = find_router(
        &state.routers,
        forwarded_host.as_deref(),
        host.as_deref(),
        authority.as_deref(),
    )
    .await;

    if let Some(mut router) = router {
        call_router_with_new_request(&mut router, request).await
    } else {
        no_router_response(
            forwarded_host.as_deref(),
            host.as_deref(),
            authority.as_deref(),
            request.version(),
        )
    }
}

fn forwarded_host<B>(request: &Request<B>) -> Option<String> {
    request
        .headers()
        .get(FORWARDED)
        .and_then(|v| v.to_str().ok())
        .and_then(|fh| {
            fh.split([',', ';'])
                .map(str::trim)
                .find_map(|parameter| {
                    let (key, value) = parameter.split_once('=')?;
                    key.trim()
                        .eq_ignore_ascii_case("host")
                        .then(|| value.trim())
                })
                .map(|host| host.trim_matches('"'))
                .filter(|host| !host.is_empty())
                .map(|host| {
                    host.strip_prefix('[')
                        .and_then(|host| host.strip_suffix(']'))
                        .unwrap_or(host)
                        .to_string()
                })
        })
}

async fn find_router(
    routers: &Arc<RwLock<HashMap<String, Router>>>,
    forwarded_host: Option<&str>,
    host: Option<&str>,
    authority: Option<&str>,
) -> Option<Router> {
    let routers = routers.read().await;
    let candidates = match forwarded_host {
        Some(forwarded) => [Some(forwarded), None],
        None => [host, authority],
    };
    candidates
        .into_iter()
        .flatten()
        .find_map(|candidate| {
            routers.get(candidate).or_else(|| {
                let authority = candidate.parse::<Authority>().ok()?;
                let host = authority.host();
                let host = host
                    .strip_prefix('[')
                    .and_then(|host| host.strip_suffix(']'))
                    .unwrap_or(host);
                routers.get(host)
            })
        })
        .or_else(|| routers.get(""))
        .cloned()
}

fn no_router_response(
    forwarded_host: Option<&str>,
    host: Option<&str>,
    authority: Option<&str>,
    version: Version,
) -> Response {
    let err = format!(
        "no router configured for forwarded_host/host/authority: {forwarded_host:?}/{host:?}/{authority:?}"
    );
    tracing::info!(
        forwarded_host = ?forwarded_host,
        host = ?host,
        authority = ?authority,
        "No router is configured for request",
    );
    let mut response = Response::builder().status(StatusCode::NOT_FOUND);
    if matches!(version, Version::HTTP_10 | Version::HTTP_11) {
        // A reused TCP flow may still reach the previous owner of a leased IP.
        // Connection is hop-by-hop and forbidden in HTTP/2.
        response = response.header(CONNECTION, "close");
    }
    response.body(err.into()).unwrap()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, Version};
    use axum::routing::get;
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    use super::authority_router;

    #[tokio::test]
    async fn missing_authority_retires_http1_connections_only() {
        let router = authority_router(Arc::new(RwLock::new(HashMap::new())));
        for (version, expected) in [
            (Version::HTTP_10, Some("close")),
            (Version::HTTP_11, Some("close")),
            (Version::HTTP_2, None),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/redfish/v1")
                        .header("host", "172.20.0.20")
                        .version(version)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{version:?}");
            assert_eq!(
                response
                    .headers()
                    .get("connection")
                    .map(|v| v.to_str().unwrap()),
                expected,
                "{version:?}"
            );
        }
    }

    #[tokio::test]
    async fn nested_authority_routers_preserve_request_version() {
        let inner = authority_router(Arc::new(RwLock::new(HashMap::new())));
        let router = authority_router(Arc::new(RwLock::new(HashMap::from([(
            String::new(),
            inner,
        )]))));
        for (version, expected) in [(Version::HTTP_11, Some("close")), (Version::HTTP_2, None)] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/redfish/v1")
                        .header("host", "172.20.0.20")
                        .version(version)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{version:?}");
            assert_eq!(
                response
                    .headers()
                    .get("connection")
                    .map(|v| v.to_str().unwrap()),
                expected,
                "{version:?}"
            );
        }
    }

    #[tokio::test]
    async fn known_authority_preserves_resource_not_found_connection_behavior() {
        let router = authority_router(Arc::new(RwLock::new(HashMap::from([(
            "172.20.0.20".to_string(),
            Router::new().route("/redfish/v1", get(|| async { StatusCode::NOT_FOUND })),
        )]))));
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/redfish/v1")
                    .header("host", "172.20.0.20")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!response.headers().contains_key("connection"));
    }

    #[tokio::test]
    async fn forwarded_miss_does_not_reach_transport_authorities() {
        let router = authority_router(Arc::new(RwLock::new(HashMap::from([
            (
                "127.0.0.1".to_string(),
                Router::new().route("/redfish/v1", get(|| async { "transport host" })),
            ),
            (
                "127.0.0.2".to_string(),
                Router::new().route("/redfish/v1", get(|| async { "transport authority" })),
            ),
        ]))));
        for (scenario, uri, host) in [
            (
                "Host and URI both name other BMCs",
                "https://127.0.0.2:8443/redfish/v1",
                Some("127.0.0.1:8443"),
            ),
            (
                "URI alone names another BMC",
                "https://127.0.0.1:8443/redfish/v1",
                None,
            ),
        ] {
            let mut request = Request::builder()
                .uri(uri)
                .header("forwarded", "host=127.0.0.4");
            if let Some(host) = host {
                request = request.header("host", host);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{scenario}");
        }
    }

    #[tokio::test]
    async fn routes_by_forwarded_host() {
        for (scenario, router_key, forwarded) in [
            ("IPv4 token", "172.20.0.20", "host=172.20.0.20"),
            (
                "case-insensitive host parameter",
                "172.20.0.20",
                "Host=172.20.0.20",
            ),
            (
                "quoted IPv6 host",
                "2001:db8::20",
                "host=\"[2001:db8::20]\"",
            ),
            (
                "host in a later forwarded element",
                "2001:db8::20",
                "for=192.0.2.1, host=\"[2001:db8::20]\"",
            ),
        ] {
            let router = authority_router(Arc::new(RwLock::new(HashMap::from([(
                router_key.to_string(),
                Router::new().route("/redfish/v1", get(|| async { "bmc" })),
            )]))));

            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/redfish/v1")
                        .header("forwarded", forwarded)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK, "{scenario}");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], b"bmc", "{scenario}");
        }
    }

    #[tokio::test]
    async fn routes_by_host_without_port_preserving_precedence() {
        let router = authority_router(Arc::new(RwLock::new(HashMap::from([
            (
                "172.20.0.20".to_string(),
                Router::new().route("/redfish/v1", get(|| async { "ipv4" })),
            ),
            (
                "2001:db8::20".to_string(),
                Router::new().route("/redfish/v1", get(|| async { "ipv6" })),
            ),
            (
                "[2001:db8::21]".to_string(),
                Router::new().route("/redfish/v1", get(|| async { "bracketed ipv6" })),
            ),
            (
                "172.20.0.20:8443".to_string(),
                Router::new().route("/redfish/v1", get(|| async { "exact" })),
            ),
        ]))));

        for (scenario, uri, forwarded, host, expected) in [
            (
                "empty Forwarded host",
                "/redfish/v1",
                Some("host="),
                Some("172.20.0.20:443"),
                "ipv4",
            ),
            (
                "quoted empty Forwarded host",
                "/redfish/v1",
                Some("host=\"\""),
                Some("172.20.0.20:443"),
                "ipv4",
            ),
            (
                "bracketed IPv6 exact key",
                "/redfish/v1",
                None,
                Some("[2001:db8::21]"),
                "bracketed ipv6",
            ),
            (
                "unparseable Host permits URI lookup",
                "https://172.20.0.20:443/redfish/v1",
                None,
                Some("not a valid authority"),
                "ipv4",
            ),
            (
                "Host with HTTPS port",
                "/redfish/v1",
                None,
                Some("172.20.0.20:443"),
                "ipv4",
            ),
            (
                "Forwarded with IPv6 and HTTPS port",
                "/redfish/v1",
                Some("host=\"[2001:db8::20]:443\""),
                None,
                "ipv6",
            ),
            (
                "URI authority with IPv6 and HTTPS port",
                "https://[2001:db8::20]:443/redfish/v1",
                None,
                None,
                "ipv6",
            ),
            (
                "exact authority wins over bare host",
                "/redfish/v1",
                None,
                Some("172.20.0.20:8443"),
                "exact",
            ),
            (
                "Forwarded fallback wins over exact Host",
                "/redfish/v1",
                Some("host=172.20.0.20:443"),
                Some("172.20.0.20:8443"),
                "ipv4",
            ),
            (
                "Host miss falls through to URI without a Forwarded host",
                "https://172.20.0.20:443/redfish/v1",
                None,
                Some("172.20.0.99:443"),
                "ipv4",
            ),
            (
                "Forwarded without host permits Host lookup",
                "/redfish/v1",
                Some("for=192.0.2.1"),
                Some("172.20.0.20:443"),
                "ipv4",
            ),
        ] {
            let mut request = Request::builder().uri(uri);
            if let Some(forwarded) = forwarded {
                request = request.header("forwarded", forwarded);
            }
            if let Some(host) = host {
                request = request.header("host", host);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK, "{scenario}");
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], expected.as_bytes(), "{scenario}");
        }
    }

    #[tokio::test]
    async fn preserves_empty_key_fallback() {
        let router = authority_router(Arc::new(RwLock::new(HashMap::from([
            (
                "".to_string(),
                Router::new().route("/redfish/v1", get(|| async { "default" })),
            ),
            (
                "127.0.0.1".to_string(),
                Router::new().route("/redfish/v1", get(|| async { "transport host" })),
            ),
        ]))));
        for forwarded in [None, Some("host=127.0.0.4")] {
            let mut request = Request::builder().uri("/redfish/v1");
            if let Some(forwarded) = forwarded {
                request = request
                    .header("forwarded", forwarded)
                    .header("host", "127.0.0.1:8443");
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], b"default");
        }
    }
}
