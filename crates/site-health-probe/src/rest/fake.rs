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

//! A fake nico-rest-api for reader tests: serves list routes with an
//! `X-Pagination` total, records every request, and can delay, push back or
//! answer a fixed status per resource.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use reqwest::StatusCode;
use serde_json::Value;
use tokio::time::Instant;

use crate::gate::CallGate;
use crate::rest::auth::Auth;
use crate::rest::client::{RestClient, new_http_client};

/// A forced reply status, with a `Retry-After` in seconds.
type Forced = (StatusCode, Option<u64>);

#[derive(Default)]
struct FakeState {
    requests: Mutex<Vec<(String, String, String)>>,
    totals: Mutex<BTreeMap<String, u64>>,
    pages: Mutex<BTreeMap<String, Vec<Vec<Value>>>>,
    objects: Mutex<BTreeMap<String, Value>>,
    queued: Mutex<BTreeMap<String, VecDeque<Forced>>>,
    always: Mutex<BTreeMap<String, StatusCode>>,
    omit_header: AtomicBool,
    delay: Mutex<Duration>,
    starts: Mutex<Vec<Instant>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

/// A running fake REST API and a client for it (10 calls/s gate).
pub(crate) struct FakeRest {
    state: Arc<FakeState>,
    client: Arc<RestClient>,
    auth: Arc<Auth>,
    _token: tempfile::NamedTempFile,
}

impl FakeRest {
    pub(crate) async fn start() -> Self {
        let state = Arc::new(FakeState::default());
        let app = Router::new()
            .route("/v2/org/{org}/nico/{*resource}", any(serve))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake REST");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await });
        let token = tempfile::NamedTempFile::new().expect("token file");
        std::fs::write(token.path(), "tok-1\n").expect("write token");
        let gate = Arc::new(CallGate::new(
            "nico-rest-api",
            10.0,
            Duration::from_secs(5),
            Duration::from_secs(30),
        ));
        let client = RestClient::new(
            new_http_client("").expect("http client"),
            &format!("http://{addr}"),
            gate,
        )
        .expect("client");
        Self {
            state,
            client: Arc::new(client),
            auth: Arc::new(Auth::TokenFile(token.path().to_path_buf())),
            _token: token,
        }
    }

    pub(crate) fn client(&self) -> Arc<RestClient> {
        Arc::clone(&self.client)
    }

    pub(crate) fn auth(&self) -> Arc<Auth> {
        Arc::clone(&self.auth)
    }

    /// The `X-Pagination` total of `key`: a resource, or
    /// `resource?status=<status>` for a status filter.
    pub(crate) fn set_total(&self, key: &str, total: u64) {
        self.state
            .totals
            .lock()
            .unwrap()
            .insert(key.to_string(), total);
    }

    /// The items of each page of `resource`, from page 1.
    pub(crate) fn set_pages(&self, resource: &str, pages: Vec<Vec<Value>>) {
        self.state
            .pages
            .lock()
            .unwrap()
            .insert(resource.to_string(), pages);
    }

    /// `resource` answers this JSON object instead of a list.
    pub(crate) fn set_object(&self, resource: &str, object: Value) {
        self.state
            .objects
            .lock()
            .unwrap()
            .insert(resource.to_string(), object);
    }

    /// The next request for `resource` gets `status`, with a `Retry-After`
    /// in seconds when given.
    pub(crate) fn queue_status(
        &self,
        resource: &str,
        status: StatusCode,
        retry_after: Option<u64>,
    ) {
        self.state
            .queued
            .lock()
            .unwrap()
            .entry(resource.to_string())
            .or_default()
            .push_back((status, retry_after));
    }

    /// Every request for `resource` gets `status`.
    pub(crate) fn always_status(&self, resource: &str, status: StatusCode) {
        self.state
            .always
            .lock()
            .unwrap()
            .insert(resource.to_string(), status);
    }

    pub(crate) fn omit_pagination_header(&self, omit: bool) {
        self.state.omit_header.store(omit, Ordering::SeqCst);
    }

    pub(crate) fn set_delay(&self, delay: Duration) {
        *self.state.delay.lock().unwrap() = delay;
    }

    /// Every request so far: method, path and raw query.
    pub(crate) fn requests(&self) -> Vec<(String, String, String)> {
        self.state.requests.lock().unwrap().clone()
    }

    pub(crate) fn starts(&self) -> Vec<Instant> {
        self.state.starts.lock().unwrap().clone()
    }

    pub(crate) fn max_in_flight(&self) -> usize {
        self.state.max_in_flight.load(Ordering::SeqCst)
    }
}

async fn serve(
    State(state): State<Arc<FakeState>>,
    method: Method,
    Path((org, resource)): Path<(String, String)>,
    RawQuery(query): RawQuery,
) -> Response {
    let query = query.unwrap_or_default();
    state.requests.lock().unwrap().push((
        method.to_string(),
        format!("/v2/org/{org}/nico/{resource}"),
        query.clone(),
    ));
    state.starts.lock().unwrap().push(Instant::now());
    let now = state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    state.max_in_flight.fetch_max(now, Ordering::SeqCst);
    let delay = *state.delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    let response = answer(&state, &resource, &query);
    state.in_flight.fetch_sub(1, Ordering::SeqCst);
    response
}

fn answer(state: &FakeState, resource: &str, query: &str) -> Response {
    let queued = state
        .queued
        .lock()
        .unwrap()
        .get_mut(resource)
        .and_then(VecDeque::pop_front);
    if let Some((status, retry_after)) = queued {
        let mut response = status.into_response();
        if let Some(secs) = retry_after {
            response
                .headers_mut()
                .insert("Retry-After", HeaderValue::from(secs));
        }
        return response;
    }
    if let Some(status) = state.always.lock().unwrap().get(resource) {
        return status.into_response();
    }
    if let Some(object) = state.objects.lock().unwrap().get(resource) {
        return axum::Json(object.clone()).into_response();
    }
    let params: BTreeMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let page_number: usize = params
        .get("pageNumber")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    let items = state
        .pages
        .lock()
        .unwrap()
        .get(resource)
        .and_then(|pages| pages.get(page_number - 1).cloned())
        .unwrap_or_default();
    let key = match params.get("status") {
        Some(status) => format!("{resource}?status={status}"),
        None => resource.to_string(),
    };
    let total = state
        .totals
        .lock()
        .unwrap()
        .get(&key)
        .copied()
        .unwrap_or(items.len() as u64);
    let mut response = axum::Json(Value::Array(items)).into_response();
    if !state.omit_header.load(Ordering::SeqCst) {
        let page = serde_json::json!({
            "pageNumber": page_number,
            "pageSize": params.get("pageSize").and_then(|p| p.parse::<u64>().ok()).unwrap_or(20),
            "total": total,
            "orderBy": null,
        });
        response.headers_mut().insert(
            "X-Pagination",
            HeaderValue::from_str(&page.to_string()).expect("header"),
        );
    }
    response
}
