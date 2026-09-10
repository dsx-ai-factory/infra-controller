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

use std::borrow::Borrow;
use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{FORWARDED, HOST};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::any;
use tokio::sync::RwLock;

use crate::router::call_router_with_new_request;

/// Multiplexes Axum routers on a single listener using request authority.
///
/// The lookup order is the `host` parameter of `Forwarded`, the `Host`
/// header, the URI authority, and finally the router stored under an empty
/// string.
///
/// Map values only have to borrow as the router to dispatch to, so callers
/// can keep bookkeeping about each entry next to its router.
pub fn authority_router<R>(routers: Arc<RwLock<HashMap<String, R>>>) -> Router
where
    R: Borrow<Router> + Send + Sync + 'static,
{
    Router::new()
        .route("/{*all}", any(process::<R>))
        .with_state(AuthorityRouter { routers })
}

struct AuthorityRouter<R> {
    routers: Arc<RwLock<HashMap<String, R>>>,
}

impl<R> Clone for AuthorityRouter<R> {
    fn clone(&self) -> Self {
        Self {
            routers: self.routers.clone(),
        }
    }
}

async fn process<R>(State(state): State<AuthorityRouter<R>>, request: Request<Body>) -> Response
where
    R: Borrow<Router> + Send + Sync + 'static,
{
    let forwarded_host = forwarded_host(&request);
    let host = request
        .headers()
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
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
                .map(|host| {
                    host.strip_prefix('[')
                        .and_then(|host| host.strip_suffix(']'))
                        .unwrap_or(host)
                        .to_string()
                })
        })
}

async fn find_router<R>(
    routers: &RwLock<HashMap<String, R>>,
    forwarded_host: Option<&str>,
    host: Option<&str>,
    authority: Option<&str>,
) -> Option<Router>
where
    R: Borrow<Router>,
{
    let routers = routers.read().await;
    let lookup = |key: &str| {
        routers
            .get(key)
            .map(|entry| Borrow::<Router>::borrow(entry).clone())
    };
    forwarded_host
        .and_then(lookup)
        .or_else(|| host.and_then(lookup))
        .or_else(|| authority.and_then(lookup))
        .or_else(|| lookup(""))
}

fn no_router_response(
    forwarded_host: Option<&str>,
    host: Option<&str>,
    authority: Option<&str>,
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
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(err.into())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    use super::authority_router;

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
    async fn preserves_empty_key_fallback() {
        let router = authority_router(Arc::new(RwLock::new(HashMap::from([(
            "".to_string(),
            Router::new().route("/redfish/v1", get(|| async { "default" })),
        )]))));

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/redfish/v1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"default");
    }
}
