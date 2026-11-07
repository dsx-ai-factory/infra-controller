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

//! Probes against nico-rest-api, on the HTTP client and Keycloak
//! client-credentials machinery of [`crate::rest`]. Adding a REST probe means
//! adding to this module — nothing in the framework or the nico-api module
//! changes.
//!
//! TODO(#5360-followup): instance-creation stat collection belongs here as
//! further read-only REST probes.

use std::sync::Arc;
use std::time::Duration;

use eyre::eyre;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use url::Url;

use crate::config;
use crate::framework::{ObservationRecorder, Probe};
use crate::gate::{CallError, CallGate, classify_http};
use crate::rest::auth::TokenSource;
use crate::rest::client::new_http_client;

/// Probes one nico-rest-api read endpoint (GET /v2/org/{org}/nico/<path>)
/// with an org-scoped bearer token obtained via client credentials. The GET
/// passes the nico-rest-api gate shared with the state readers; the token
/// request does not, it goes to Keycloak.
pub(crate) struct ReadProbe {
    name: &'static str,
    operation: &'static str,
    path: &'static str,
    cfg: config::RestProbe,
    client: reqwest::Client,
    tokens: TokenSource,
    gate: Arc<CallGate>,
}

/// Probes the machines read endpoint.
pub(crate) fn new_machines_probe(
    cfg: config::RestProbe,
    gate: Arc<CallGate>,
) -> eyre::Result<ReadProbe> {
    new_read_probe("rest_machines", "machine", "get_machine", cfg, gate)
}

/// Probes the instances read endpoint.
pub(crate) fn new_instances_probe(
    cfg: config::RestProbe,
    gate: Arc<CallGate>,
) -> eyre::Result<ReadProbe> {
    new_read_probe("rest_instances", "instance", "get_instance", cfg, gate)
}

fn new_read_probe(
    name: &'static str,
    path: &'static str,
    operation: &'static str,
    cfg: config::RestProbe,
    gate: Arc<CallGate>,
) -> eyre::Result<ReadProbe> {
    let client = new_http_client(&cfg.ca)?;
    Ok(ReadProbe {
        name,
        operation,
        path,
        tokens: TokenSource::new((&cfg.auth).into(), client.clone()),
        cfg,
        client,
        gate,
    })
}

#[async_trait::async_trait]
impl Probe for ReadProbe {
    fn name(&self) -> &'static str {
        self.name
    }

    fn api(&self) -> &'static str {
        self.gate.api()
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

        self.gate
            .call(recorder, self.operation, || async {
                let response = self
                    .client
                    .get(endpoint)
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(ACCEPT, "application/json")
                    .send()
                    .await
                    .map_err(|e| CallError::Transport(format!("GET {}: {e}", self.path)))?;
                let status = response.status();
                if status == reqwest::StatusCode::UNAUTHORIZED {
                    // Token may have been revoked server-side; drop the cache
                    // so the next run re-authenticates instead of failing
                    // forever. This runs before the drain below, whose own
                    // error would otherwise return early and leave the revoked
                    // token cached until its expiry margin.
                    self.tokens.drop_cached();
                }
                let pushback = match classify_http(status, response.headers()) {
                    Some(err @ CallError::Pushback { .. }) => Some(err),
                    _ => None,
                };
                // Drain fully so time-to-last-byte is what the histogram
                // records, not time-to-first-header. Deliberately no byte cap:
                // a cap silently truncates large sites' machine lists (success
                // recorded, wrong duration), while the gate's call timeout
                // already bounds how long this read may take — an oversized
                // response surfaces as an honest timeout instead.
                drain(response)
                    .await
                    .map_err(|e| CallError::Transport(format!("read body: {e}")))?;
                if let Some(pushback) = pushback {
                    return Err(pushback);
                }
                if status != reqwest::StatusCode::OK {
                    return Err(CallError::Status(format!(
                        "GET {} returned {}",
                        self.path,
                        status.as_u16()
                    )));
                }
                Ok(())
            })
            .await
            .map_err(eyre::Report::new)
    }
}

async fn drain(mut response: reqwest::Response) -> reqwest::Result<()> {
    while response.chunk().await?.is_some() {}
    Ok(())
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
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::config::RestAuth;
    use crate::framework::{Outcome, start};

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
        /// How long the API waits before it answers.
        api_delay: std::sync::Mutex<Duration>,
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
        let delay = *site.api_delay.lock().expect("mutex");
        tokio::time::sleep(delay).await;
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
        harness_with_call_timeout(Duration::from_secs(5)).await
    }

    async fn harness_with_call_timeout(call_timeout: Duration) -> Harness {
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
        let gate = Arc::new(CallGate::new(
            "nico-rest-api",
            100.0,
            call_timeout,
            Duration::from_secs(30),
        ));
        Harness {
            site,
            probe: new_machines_probe(cfg, gate).expect("probe builds"),
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

    /// A call cut by the gate's call timeout, well before the probe's own
    /// 5 s deadline, is a probe timeout, not a failure.
    #[tokio::test]
    async fn gate_call_timeout_is_a_probe_timeout() {
        let h = harness_with_call_timeout(Duration::from_millis(100)).await;
        *h.site.api_delay.lock().expect("mutex") = Duration::from_secs(2);

        let cancel = CancellationToken::new();
        let (mut results, _loops) = start(vec![Arc::new(h.probe)], cancel.clone());
        let result = results.recv().await.expect("result");
        cancel.cancel();

        assert_eq!(result.outcome, Outcome::Timeout);
        assert_eq!(result.error.as_deref(), Some("call timed out"));
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
