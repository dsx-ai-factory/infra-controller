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

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use axum_extra::TypedHeader;
use axum_extra::headers::Authorization;
use axum_extra::headers::authorization::Basic;
use rand::RngExt;
use serde_json::json;

use crate::bmc_state::BmcState;
use crate::json::JsonExt;
use crate::{http, redfish};

const X_AUTH_TOKEN: HeaderName = HeaderName::from_static("x-auth-token");
const SESSION_TOKEN_TTL: Duration = Duration::from_secs(120);

pub(super) fn service_resource() -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Borrowed("/redfish/v1/SessionService"),
        odata_type: Cow::Borrowed("#SessionService.v1_1_9.SessionService"),
        id: Cow::Borrowed("SessionService"),
        name: Cow::Borrowed("Session Service"),
    }
}

pub(crate) fn sessions_collection() -> redfish::Collection<'static> {
    redfish::Collection {
        odata_id: Cow::Borrowed("/redfish/v1/SessionService/Sessions"),
        odata_type: Cow::Borrowed("#SessionCollection.SessionCollection"),
        name: Cow::Borrowed("Session Collection"),
    }
}

fn session_resource(id: impl Display) -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Owned(format!("/redfish/v1/SessionService/Sessions/{id}")),
        odata_type: Cow::Borrowed("#Session.v1_7_0.Session"),
        id: Cow::Owned(id.to_string()),
        name: Cow::Borrowed("User Session"),
    }
}

pub(crate) fn add_routes(r: Router<BmcState>) -> Router<BmcState> {
    r.route(&service_resource().odata_id, get(get_service))
        .route(
            &sessions_collection().odata_id,
            get(get_sessions).post(post_session),
        )
        .route(
            format!("{}/{{session_id}}", sessions_collection().odata_id).as_str(),
            get(get_session).delete(delete_session),
        )
}

#[derive(Clone, Debug)]
pub(crate) struct SessionRecord {
    pub(crate) id: String,
    pub(crate) username: String,
    pub(crate) token: String,
    pub(crate) expires_at: Instant,
}

impl SessionRecord {
    fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }

    fn to_json(&self) -> serde_json::Value {
        json!({
            "UserName": self.username,
            "SessionType": "Redfish",
        })
        .patch(session_resource(&self.id))
    }
}

#[derive(Debug, Default)]
pub(crate) struct SessionServiceState {
    next_id: Mutex<u64>,
    sessions: Mutex<HashMap<String, SessionRecord>>,
}

impl SessionServiceState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_token_valid(&self, token: &str) -> bool {
        let mut sessions = self.sessions.lock().expect("mutex poisoned");
        Self::prune_expired(&mut sessions, Instant::now());
        sessions.contains_key(token)
    }

    pub(crate) fn create(&self, username: impl Into<String>) -> SessionRecord {
        let id = {
            let mut next_id = self.next_id.lock().expect("mutex poisoned");
            *next_id += 1;
            next_id.to_string()
        };
        let token = generate_token();
        let record = SessionRecord {
            id,
            username: username.into(),
            token: token.clone(),
            expires_at: Instant::now() + SESSION_TOKEN_TTL,
        };
        self.sessions
            .lock()
            .expect("mutex poisoned")
            .insert(token, record.clone());
        record
    }

    pub(crate) fn list(&self) -> Vec<SessionRecord> {
        let mut sessions = self.sessions.lock().expect("mutex poisoned");
        Self::prune_expired(&mut sessions, Instant::now());
        sessions.values().cloned().collect()
    }

    pub(crate) fn find_by_id(&self, id: &str) -> Option<SessionRecord> {
        let mut sessions = self.sessions.lock().expect("mutex poisoned");
        Self::prune_expired(&mut sessions, Instant::now());
        sessions.values().find(|rec| rec.id == id).cloned()
    }

    pub(crate) fn delete_by_id(&self, id: &str) -> bool {
        let mut sessions = self.sessions.lock().expect("mutex poisoned");
        Self::prune_expired(&mut sessions, Instant::now());
        let Some(token) = sessions
            .iter()
            .find_map(|(tok, rec)| (rec.id == id).then(|| tok.clone()))
        else {
            return false;
        };
        sessions.remove(&token);
        true
    }

    fn prune_expired(sessions: &mut HashMap<String, SessionRecord>, now: Instant) {
        sessions.retain(|_, rec| !rec.is_expired(now));
    }
}

/// 32 lowercase hex characters from 128 random bits.
pub(super) fn generate_token() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    bytes.iter().fold(String::with_capacity(32), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

async fn get_service() -> Response {
    json!({
        "ServiceEnabled": true,
        "SessionTimeout": SESSION_TOKEN_TTL.as_secs(),
        "Sessions": {
            "@odata.id": sessions_collection().odata_id,
        },
    })
    .patch(service_resource())
    .into_ok_response()
}

async fn get_sessions(State(state): State<BmcState>) -> Response {
    let members = state
        .session_service_state
        .list()
        .iter()
        .map(|rec| session_resource(&rec.id).entity_ref())
        .collect::<Vec<_>>();
    sessions_collection()
        .with_members(&members)
        .into_ok_response()
}

async fn get_session(State(state): State<BmcState>, Path(session_id): Path<String>) -> Response {
    state
        .session_service_state
        .find_by_id(&session_id)
        .map(|rec| rec.to_json().into_ok_response())
        .unwrap_or_else(http::not_found)
}

/// `POST /redfish/v1/SessionService/Sessions`.
async fn post_session(
    State(state): State<BmcState>,
    authorization: Option<TypedHeader<Authorization<Basic>>>,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    let body_creds = body.as_ref().and_then(|Json(value)| {
        let username = value.get("UserName").and_then(serde_json::Value::as_str)?;
        let password = value.get("Password").and_then(serde_json::Value::as_str)?;
        Some((username.to_string(), password.to_string()))
    });
    let basic_creds = authorization
        .as_ref()
        .map(|TypedHeader(Authorization(basic))| {
            (basic.username().to_string(), basic.password().to_string())
        });

    let Some((username, password)) = body_creds.or(basic_creds) else {
        tracing::warn!("Session creation request without credentials");
        return StatusCode::BAD_REQUEST.into_response();
    };

    if !state
        .account_service_state
        .is_authorized(&username, &password)
    {
        tracing::warn!(username, "Session creation rejected: bad credentials");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let record = state.session_service_state.create(username);
    let location = session_resource(&record.id).odata_id.into_owned();
    let mut response = record
        .to_json()
        .into_response(StatusCode::CREATED)
        .into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&location) {
        headers.insert(axum::http::header::LOCATION, value);
    }
    if let Ok(value) = HeaderValue::from_str(&record.token) {
        headers.insert(X_AUTH_TOKEN, value);
    }
    response
}

async fn delete_session(State(state): State<BmcState>, Path(session_id): Path<String>) -> Response {
    if state.session_service_state.delete_by_id(&session_id) {
        http::ok_no_content()
    } else {
        http::not_found()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_assigns_unique_ids_and_tokens() {
        let state = SessionServiceState::new();
        let a = state.create("root");
        let b = state.create("root");
        assert_ne!(a.id, b.id);
        assert_ne!(a.token, b.token);
        assert!(state.is_token_valid(&a.token));
        assert!(state.is_token_valid(&b.token));
    }

    #[test]
    fn delete_by_id_removes_token() {
        let state = SessionServiceState::new();
        let rec = state.create("root");
        assert!(state.delete_by_id(&rec.id));
        assert!(!state.is_token_valid(&rec.token));
        assert!(!state.delete_by_id(&rec.id));
    }

    #[test]
    fn list_returns_outstanding_sessions() {
        let state = SessionServiceState::new();
        state.create("a");
        state.create("b");
        assert_eq!(state.list().len(), 2);
    }

    #[test]
    fn expired_tokens_are_invalid_and_pruned() {
        let state = SessionServiceState::new();
        let rec = state.create("root");
        {
            let mut sessions = state.sessions.lock().expect("mutex poisoned");
            sessions
                .get_mut(&rec.token)
                .expect("session exists")
                .expires_at = Instant::now() - Duration::from_secs(1);
        }

        assert!(!state.is_token_valid(&rec.token));
        assert!(state.find_by_id(&rec.id).is_none());
        assert!(state.list().is_empty());
    }
}
