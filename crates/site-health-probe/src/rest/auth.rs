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

//! Bearer tokens for nico-rest-api: a Keycloak client-credentials token
//! cached until shortly before expiry, or a mounted token file re-read on
//! every call. Tokens and secrets are never logged.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;

use crate::config;
use crate::gate::{CallError, classify_http};
use crate::rest::client::read_capped;

/// The Keycloak token response is small; a response bigger than this is not a
/// token endpoint.
const TOKEN_RESPONSE_CAP: usize = 1 << 20;

/// Refresh the cached token once it is this close to expiry.
const TOKEN_EXPIRY_MARGIN: Duration = Duration::from_secs(30);

/// Cap on how far ahead a token expiry is tracked. A token endpoint can claim
/// an arbitrarily large `expires_in`; uncapped, the `Instant` addition below
/// could overflow and panic the run. The cache only needs "far future".
const TOKEN_LIFETIME_CAP: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Where a client-credentials token comes from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClientCredentials {
    pub token_url: String,
    pub client_id: String,
    pub client_secret_path: String,
}

impl From<&config::RestAuth> for ClientCredentials {
    fn from(auth: &config::RestAuth) -> Self {
        Self {
            token_url: auth.token_url.clone(),
            client_id: auth.client_id.clone(),
            client_secret_path: auth.client_secret_path.clone(),
        }
    }
}

/// How a REST reader authenticates as one org identity.
pub(crate) enum Auth {
    ClientCredentials(TokenSource),
    TokenFile(PathBuf),
}

impl Auth {
    /// Builds the identity a validated config names.
    pub(crate) fn new(creds: &config::RestCredentials, client: reqwest::Client) -> Self {
        if creds.token_file.is_empty() {
            Auth::ClientCredentials(TokenSource::new(
                ClientCredentials {
                    token_url: creds.token_url.clone(),
                    client_id: creds.client_id.clone(),
                    client_secret_path: creds.client_secret_path.clone(),
                },
                client,
            ))
        } else {
            Auth::TokenFile(PathBuf::from(&creds.token_file))
        }
    }

    /// The bearer token for the next call.
    pub(crate) async fn bearer(&self) -> Result<String, CallError> {
        match self {
            Auth::ClientCredentials(tokens) => tokens.bearer().await,
            Auth::TokenFile(path) => {
                let token = tokio::fs::read_to_string(path)
                    .await
                    .map_err(|e| CallError::Transport(format!("read token file: {e}")))?;
                let token = token.trim();
                if token.is_empty() {
                    return Err(CallError::Transport("token file is empty".to_string()));
                }
                Ok(token.to_string())
            }
        }
    }

    /// The API refused the token: forget a cached one so the next call
    /// re-authenticates.
    pub(crate) fn rejected(&self) {
        if let Auth::ClientCredentials(tokens) = self {
            tokens.drop_cached();
        }
    }
}

/// Caches a Keycloak client-credentials bearer token and refreshes it shortly
/// before expiry. The client secret is re-read from its mounted file on every
/// refresh (rotation-safe). Tokens and secrets are never logged.
pub(crate) struct TokenSource {
    auth: ClientCredentials,
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
    pub(crate) fn new(auth: ClientCredentials, client: reqwest::Client) -> Self {
        Self {
            auth,
            client,
            cached: std::sync::Mutex::new(None),
        }
    }

    /// Returns a valid token, refreshing when within the expiry margin. A
    /// token endpoint that cannot be reached or answers 429 or 5xx is worth
    /// retrying, any other refusal (a 401 for a bad secret, say) is not.
    pub(crate) async fn bearer(&self) -> Result<String, CallError> {
        if let Some(cached) = self.cached_token() {
            return Ok(cached);
        }

        let secret = tokio::fs::read_to_string(&self.auth.client_secret_path)
            .await
            .map_err(|e| CallError::Transport(format!("read client secret: {e}")))?;
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
            .map_err(|e| CallError::Transport(format!("token request: {e}")))?;
        let status = response.status();
        // Deliberately not echoing the body of a refusal: error bodies from
        // misconfigured IdPs can include sensitive detail.
        let refused = format!("token endpoint returned {}", status.as_u16());
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(
                classify_http(status, response.headers()).unwrap_or(CallError::Transport(refused))
            );
        }
        if status.is_server_error() {
            return Err(CallError::Transport(refused));
        }
        if status != StatusCode::OK {
            return Err(CallError::Status(refused));
        }
        let body = read_capped(response, TOKEN_RESPONSE_CAP)
            .await
            .map_err(|e| CallError::Transport(format!("read token response: {e}")))?;
        let parsed: TokenResponse = serde_json::from_slice(&body)
            .map_err(|e| CallError::Decode(format!("parse token response: {e}")))?;
        if parsed.access_token.is_empty() {
            return Err(CallError::Decode(
                "token endpoint returned no access_token".to_string(),
            ));
        }
        let expiry = Instant::now()
            + Duration::from_secs(parsed.expires_in.try_into().unwrap_or(0))
                .min(TOKEN_LIFETIME_CAP);
        *self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CachedToken {
            token: parsed.access_token.clone(),
            expiry,
        });
        Ok(parsed.access_token)
    }

    fn cached_token(&self) -> Option<String> {
        let cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cached
            .as_ref()
            .filter(|c| c.expiry.saturating_duration_since(Instant::now()) > TOKEN_EXPIRY_MARGIN)
            .map(|c| c.token.clone())
    }

    /// Discards the cached token so the next call re-authenticates.
    pub(crate) fn drop_cached(&self) {
        *self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};

    use axum::Router;
    use axum::extract::State;
    use axum::routing::{get, post};

    use super::*;
    use crate::framework::ObservationRecorder;
    use crate::gate::{CallGate, MAX_RETRIES};
    use crate::rest::client::{RestClient, new_http_client};

    /// A fake token endpoint that answers `token_status`, and an API that
    /// counts the GETs that reach it.
    #[derive(Default)]
    struct FakeIdp {
        token_status: AtomicU16,
        token_requests: AtomicU32,
        api_requests: AtomicU32,
    }

    /// A REST client and a client-credentials identity wired to a fake IdP
    /// that answers `token_status`.
    async fn idp_with(
        token_status: u16,
    ) -> (Arc<FakeIdp>, RestClient, Auth, tempfile::NamedTempFile) {
        let idp = Arc::new(FakeIdp::default());
        idp.token_status.store(token_status, Ordering::SeqCst);
        let app = Router::new()
            .route(
                "/token",
                post(|State(idp): State<Arc<FakeIdp>>| async move {
                    idp.token_requests.fetch_add(1, Ordering::SeqCst);
                    axum::http::StatusCode::from_u16(idp.token_status.load(Ordering::SeqCst))
                        .expect("valid status")
                }),
            )
            .route(
                "/v2/org/{org}/nico/{*path}",
                get(|State(idp): State<Arc<FakeIdp>>| async move {
                    idp.api_requests.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .with_state(Arc::clone(&idp));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake idp");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });

        let secret = tempfile::NamedTempFile::new().expect("secret file");
        std::fs::write(secret.path(), "s3cr3t").expect("write secret");
        let http = new_http_client("").expect("client");
        let auth = Auth::new(
            &config::RestCredentials {
                token_url: format!("http://{addr}/token"),
                client_id: "probe".to_string(),
                client_secret_path: secret.path().to_str().expect("utf-8").to_string(),
                ..Default::default()
            },
            http.clone(),
        );
        let gate = Arc::new(CallGate::new(
            "nico-rest-api",
            100.0,
            Duration::from_secs(5),
            Duration::from_secs(30),
        ));
        let client = RestClient::new(http, &format!("http://{addr}"), gate).expect("rest client");
        (idp, client, auth, secret)
    }

    async fn get_machines(client: &RestClient, auth: &Auth) -> Result<(), CallError> {
        client
            .get(
                &ObservationRecorder::default(),
                "get_machine",
                auth,
                "org",
                &["machine"],
                &[],
            )
            .await
            .map(|_| ())
    }

    #[tokio::test]
    async fn a_refused_token_request_is_not_retried() {
        let (idp, client, auth, _secret) = idp_with(401).await;

        let err = get_machines(&client, &auth)
            .await
            .expect_err("a 401 from the token endpoint fails the call");

        assert!(!err.is_retryable(), "got: {err:?}");
        assert!(err.category().contains("401"), "got: {}", err.category());
        assert_eq!(idp.token_requests.load(Ordering::SeqCst), 1);
        assert_eq!(idp.api_requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_token_endpoint_outage_is_retried() {
        let (idp, client, auth, _secret) = idp_with(503).await;

        let err = get_machines(&client, &auth)
            .await
            .expect_err("a 503 from the token endpoint fails the call");

        assert!(err.is_retryable(), "got: {err:?}");
        assert_eq!(
            idp.token_requests.load(Ordering::SeqCst) as usize,
            1 + MAX_RETRIES
        );
        assert_eq!(idp.api_requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_token_reply_without_a_token_is_a_decode_error() {
        // A 200 with an empty body is not a token response.
        let (idp, client, auth, _secret) = idp_with(200).await;

        let err = get_machines(&client, &auth)
            .await
            .expect_err("an empty token reply fails the call");

        assert!(matches!(err, CallError::Decode(_)), "got: {err:?}");
        assert_eq!(idp.token_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn token_file_is_reread_and_credentials_pick_a_source() {
        let client = new_http_client("").expect("client");
        let file = tempfile::NamedTempFile::new().expect("token file");
        let from_file = Auth::new(
            &config::RestCredentials {
                token_file: file.path().to_str().expect("utf-8").to_string(),
                ..Default::default()
            },
            client.clone(),
        );
        std::fs::write(file.path(), "tok-a\n").expect("write");
        assert_eq!(from_file.bearer().await.expect("token"), "tok-a");
        // A rotated token is picked up on the next call.
        std::fs::write(file.path(), "tok-b").expect("write");
        assert_eq!(from_file.bearer().await.expect("token"), "tok-b");
        std::fs::write(file.path(), "  \n").expect("write");
        assert!(from_file.bearer().await.is_err(), "an empty token file");

        let from_keycloak = Auth::new(
            &config::RestCredentials {
                token_url: "https://keycloak/token".to_string(),
                client_id: "probe".to_string(),
                client_secret_path: "/secret".to_string(),
                ..Default::default()
            },
            client,
        );
        assert!(matches!(from_keycloak, Auth::ClientCredentials(_)));
    }
}
