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

//! Probes against nico-rest-api. This module owns the HTTP client and the
//! Keycloak client-credentials machinery; probes here share it. Adding a REST
//! probe means adding to this module — nothing in the framework or the
//! nico-api module changes.
//!
//! TODO(#5360-followup): instance-creation stat collection belongs here as
//! further read-only REST probes.

use std::time::{Duration, Instant};

use eyre::eyre;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use url::Url;

use crate::config;
use crate::framework::{ObservationRecorder, Probe};

/// The Keycloak token response is small; a response bigger than this is not a
/// token endpoint.
const TOKEN_RESPONSE_CAP: usize = 1 << 20;

/// Refresh the cached token once it is this close to expiry.
const TOKEN_EXPIRY_MARGIN: Duration = Duration::from_secs(30);

/// Probes one nico-rest-api read endpoint (GET /v2/org/{org}/nico/<path>)
/// with an org-scoped bearer token obtained via client credentials.
pub(crate) struct ReadProbe {
    name: &'static str,
    operation: &'static str,
    path: &'static str,
    cfg: config::RestProbe,
    client: reqwest::Client,
    tokens: TokenSource,
}

/// Probes the machines read endpoint.
pub(crate) fn new_machines_probe(cfg: config::RestProbe) -> eyre::Result<ReadProbe> {
    new_read_probe("rest_machines", "machine", "get_machine", cfg)
}

/// Probes the instances read endpoint.
pub(crate) fn new_instances_probe(cfg: config::RestProbe) -> eyre::Result<ReadProbe> {
    new_read_probe("rest_instances", "instance", "get_instance", cfg)
}

fn new_read_probe(
    name: &'static str,
    path: &'static str,
    operation: &'static str,
    cfg: config::RestProbe,
) -> eyre::Result<ReadProbe> {
    let client = new_http_client(&cfg.ca)?;
    Ok(ReadProbe {
        name,
        operation,
        path,
        tokens: TokenSource::new(cfg.auth.clone(), client.clone()),
        cfg,
        client,
    })
}

#[async_trait::async_trait]
impl Probe for ReadProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn api(&self) -> &'static str {
        "nico-rest-api"
    }

    fn interval(&self) -> Duration {
        self.cfg.interval
    }

    fn timeout(&self) -> Duration {
        self.cfg.timeout
    }

    async fn run(&self, recorder: &ObservationRecorder) -> eyre::Result<()> {
        let token = self.tokens.bearer().await?;

        let mut endpoint = Url::parse(&self.cfg.target).map_err(|e| eyre!("build request: {e}"))?;
        endpoint
            .path_segments_mut()
            .map_err(|()| eyre!("build request: target cannot be a base URL"))?
            .pop_if_empty()
            .extend(["v2", "org", &self.cfg.org, "nico", self.path]);

        let start = Instant::now();
        let response = self
            .client
            .get(endpoint)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| eyre!("GET {}: {e}", self.path))?;
        let status = response.status();
        // Drain fully so time-to-last-byte is what the histogram records, not
        // time-to-first-header. Deliberately no byte cap: a cap silently
        // truncates large sites' machine lists (success recorded, wrong
        // duration), while the run's deadline already bounds how long this
        // read may take — an oversized response surfaces as an honest timeout
        // instead.
        drain(response).await.map_err(|e| eyre!("read body: {e}"))?;
        let took = start.elapsed();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            // Token may have been revoked server-side; drop the cache so the
            // next run re-authenticates instead of failing forever.
            self.tokens.drop_cached();
        }
        if status != reqwest::StatusCode::OK {
            return Err(eyre!("GET {} returned {}", self.path, status.as_u16()));
        }
        recorder.record(self.operation, took);
        Ok(())
    }
}

async fn drain(mut response: reqwest::Response) -> reqwest::Result<()> {
    while response.chunk().await?.is_some() {}
    Ok(())
}

/// Caches a Keycloak client-credentials bearer token and refreshes it shortly
/// before expiry. The client secret is re-read from its mounted file on every
/// refresh (rotation-safe). Tokens and secrets are never logged.
struct TokenSource {
    auth: config::RestAuth,
    client: reqwest::Client,
    // Held only for cache reads and writes, never across an await. Two
    // overlapping refreshes would both hit the token endpoint; the last one
    // wins the cache, which is harmless.
    cached: std::sync::Mutex<Option<CachedToken>>,
}

struct CachedToken {
    token: String,
    expiry: Instant,
}

/// The subset of the OAuth2 token endpoint response we use.
#[derive(serde::Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    expires_in: i64,
}

impl TokenSource {
    fn new(auth: config::RestAuth, client: reqwest::Client) -> Self {
        Self {
            auth,
            client,
            cached: std::sync::Mutex::new(None),
        }
    }

    /// Returns a valid token, refreshing when within the expiry margin.
    async fn bearer(&self) -> eyre::Result<String> {
        if let Some(cached) = self.cached_token() {
            return Ok(cached);
        }

        let secret = tokio::fs::read_to_string(&self.auth.client_secret_path)
            .await
            .map_err(|e| eyre!("read client secret: {e}"))?;
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "client_credentials")
            .append_pair("client_id", &self.auth.client_id)
            .append_pair("client_secret", secret.trim())
            .finish();
        let response = self
            .client
            .post(&self.auth.token_url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form)
            .send()
            .await
            .map_err(|e| eyre!("token request: {e}"))?;
        let status = response.status();
        let body = read_capped(response, TOKEN_RESPONSE_CAP)
            .await
            .map_err(|e| eyre!("read token response: {e}"))?;
        if status != reqwest::StatusCode::OK {
            // Deliberately not echoing the body: error bodies from
            // misconfigured IdPs can include sensitive detail.
            return Err(eyre!("token endpoint returned {}", status.as_u16()));
        }
        let parsed: TokenResponse =
            serde_json::from_slice(&body).map_err(|e| eyre!("parse token response: {e}"))?;
        if parsed.access_token.is_empty() {
            return Err(eyre!("token endpoint returned no access_token"));
        }
        let expiry =
            Instant::now() + Duration::from_secs(parsed.expires_in.try_into().unwrap_or(0));
        *self.cached.lock().expect("token cache mutex") = Some(CachedToken {
            token: parsed.access_token.clone(),
            expiry,
        });
        Ok(parsed.access_token)
    }

    fn cached_token(&self) -> Option<String> {
        let cached = self.cached.lock().expect("token cache mutex");
        cached
            .as_ref()
            .filter(|c| c.expiry.saturating_duration_since(Instant::now()) > TOKEN_EXPIRY_MARGIN)
            .map(|c| c.token.clone())
    }

    /// Discards the cached token so the next call re-authenticates.
    fn drop_cached(&self) {
        *self.cached.lock().expect("token cache mutex") = None;
    }
}

/// Reads at most `cap` bytes of the response body. Only the token endpoint is
/// capped — see the drain comment in [`ReadProbe::run`] for why the probe GET
/// is not.
async fn read_capped(mut response: reqwest::Response, cap: usize) -> reqwest::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = cap - body.len();
        if chunk.len() >= remaining {
            body.extend_from_slice(&chunk[..remaining]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// An HTTP client with full TLS verification; an optional extra CA bundle
/// supports cluster-internal issuers. Redirects are refused outright: the
/// token POST carries the client secret in its body (which clients re-send on
/// 307/308 redirects, to ANY host) and the API GET carries a bearer token —
/// both endpoints are fixed cluster-internal services that have no legitimate
/// reason to redirect, so following one could only ever hand credentials to
/// somewhere unexpected.
fn new_http_client(ca_file: &str) -> eyre::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .min_tls_version(reqwest::tls::Version::TLS_1_2);
    if !ca_file.is_empty() {
        let pem = std::fs::read(ca_file).map_err(|e| eyre!("read REST CA: {e}"))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|e| eyre!("no certificates parsed from {ca_file}: {e}"))?;
        if certs.is_empty() {
            return Err(eyre!("no certificates parsed from {ca_file}"));
        }
        for cert in certs {
            // Added to the default (system) roots, not replacing them.
            builder = builder.add_root_certificate(cert);
        }
    }
    builder.build().map_err(|e| eyre!("build HTTP client: {e}"))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};

    use axum::Router;
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Json, Response};
    use axum::routing::{get, post};

    use super::*;
    use crate::config::RestAuth;

    /// Shared state of the fake Keycloak + fake API server.
    #[derive(Default)]
    struct FakeSite {
        token_requests: AtomicU32,
        api_requests: AtomicU32,
        /// Status the API returns; 0 means 200 with a JSON body.
        api_status: AtomicU16,
        /// When set, the API redirects (307) to this absolute URL.
        redirect_to: std::sync::Mutex<Option<String>>,
        /// The client_secret the token endpoint last received.
        seen_secret: std::sync::Mutex<Option<String>>,
        /// The Authorization header the API last received.
        seen_authorization: std::sync::Mutex<Option<String>>,
    }

    async fn token_handler(
        State(site): State<Arc<FakeSite>>,
        body: axum::extract::Form<std::collections::HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        site.token_requests.fetch_add(1, Ordering::SeqCst);
        *site.seen_secret.lock().expect("mutex") = body.get("client_secret").cloned();
        Json(serde_json::json!({
            "access_token": "tok-123",
            "expires_in": 3600,
            "token_type": "Bearer"
        }))
    }

    async fn api_handler(State(site): State<Arc<FakeSite>>, headers: HeaderMap) -> Response {
        site.api_requests.fetch_add(1, Ordering::SeqCst);
        *site.seen_authorization.lock().expect("mutex") = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if let Some(target) = site.redirect_to.lock().expect("mutex").clone() {
            return (
                axum::http::StatusCode::TEMPORARY_REDIRECT,
                [(axum::http::header::LOCATION, target)],
            )
                .into_response();
        }
        match site.api_status.load(Ordering::SeqCst) {
            0 => Json(serde_json::json!({"machines": []})).into_response(),
            status => axum::http::StatusCode::from_u16(status)
                .expect("valid status")
                .into_response(),
        }
    }

    async fn serve(site: Arc<FakeSite>) -> SocketAddr {
        let app = Router::new()
            .route("/token", post(token_handler))
            .route("/v2/org/{org}/nico/{path}", get(api_handler))
            .with_state(site);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake site");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        addr
    }

    struct Harness {
        site: Arc<FakeSite>,
        probe: ReadProbe,
        _secret_file: tempfile::NamedTempFile,
    }

    async fn harness() -> Harness {
        let site = Arc::new(FakeSite::default());
        let addr = serve(Arc::clone(&site)).await;
        let mut secret_file = tempfile::NamedTempFile::new().expect("secret file");
        secret_file.write_all(b"s3cr3t\n").expect("write secret");
        // Plain http is fine here: the https requirement is enforced by config
        // validation, which these constructor inputs bypass on purpose.
        let cfg = config::RestProbe {
            enabled: true,
            target: format!("http://{addr}"),
            interval: Duration::from_secs(15),
            timeout: Duration::from_secs(5),
            org: "test org".to_string(),
            auth: RestAuth {
                token_url: format!("http://{addr}/token"),
                client_id: "probe".to_string(),
                client_secret_path: secret_file.path().to_str().expect("utf-8 path").to_string(),
            },
            ca: String::new(),
        };
        Harness {
            site,
            probe: new_machines_probe(cfg).expect("probe builds"),
            _secret_file: secret_file,
        }
    }

    #[tokio::test]
    async fn happy_path_reuses_cached_token() {
        let h = harness().await;
        let recorder = ObservationRecorder::default();

        h.probe.run(&recorder).await.expect("first run succeeds");
        h.probe.run(&recorder).await.expect("second run succeeds");

        assert_eq!(
            h.site.token_requests.load(Ordering::SeqCst),
            1,
            "the token is fetched once and reused"
        );
        assert_eq!(h.site.api_requests.load(Ordering::SeqCst), 2);
        assert_eq!(
            h.site.seen_authorization.lock().expect("mutex").as_deref(),
            Some("Bearer tok-123"),
        );
        assert_eq!(
            h.site.seen_secret.lock().expect("mutex").as_deref(),
            Some("s3cr3t"),
            "the mounted secret is sent trimmed"
        );
    }

    #[tokio::test]
    async fn non_200_is_an_error() {
        let h = harness().await;
        h.site.api_status.store(500, Ordering::SeqCst);
        let err = h
            .probe
            .run(&ObservationRecorder::default())
            .await
            .expect_err("500 fails the run");
        assert!(
            err.to_string().contains("GET machine returned 500"),
            "got: {err:#}"
        );
    }

    #[tokio::test]
    async fn unauthorized_drops_the_cached_token() {
        let h = harness().await;
        let recorder = ObservationRecorder::default();
        h.probe.run(&recorder).await.expect("prime the token cache");

        h.site.api_status.store(401, Ordering::SeqCst);
        let err = h.probe.run(&recorder).await.expect_err("401 fails the run");
        assert!(
            err.to_string().contains("GET machine returned 401"),
            "got: {err:#}"
        );

        h.site.api_status.store(0, Ordering::SeqCst);
        h.probe
            .run(&recorder)
            .await
            .expect("recovers after re-auth");
        assert_eq!(
            h.site.token_requests.load(Ordering::SeqCst),
            2,
            "the 401 dropped the cache, forcing one re-authentication"
        );
    }

    #[tokio::test]
    async fn redirects_are_refused_and_not_followed() {
        let h = harness().await;
        // A second server the redirect points at; the client must never call it.
        let lure = Arc::new(FakeSite::default());
        let lure_addr = serve(Arc::clone(&lure)).await;
        *h.site.redirect_to.lock().expect("mutex") =
            Some(format!("http://{lure_addr}/v2/org/test%20org/nico/machine"));

        let err = h
            .probe
            .run(&ObservationRecorder::default())
            .await
            .expect_err("a redirect fails the run");
        assert!(
            err.to_string().contains("GET machine returned 307"),
            "got: {err:#}"
        );
        assert_eq!(
            lure.api_requests.load(Ordering::SeqCst),
            0,
            "the redirect target must never receive the bearer token"
        );
    }
}
