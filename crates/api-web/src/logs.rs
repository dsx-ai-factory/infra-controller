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

//! Admin UI live log viewer.
//!
//! `page()` serves up the unified logs hub. `stream()` is a Server-Sent Events
//! endpoint that replays the recent in-process tracing buffer and then tails live
//! events from [`carbide_api_core::LogStream`]. Only the `api` source is
//! wired in for now. The idea is this logs hub will eventually be a place where
//! we can get similar log streaming for Scout and DPU agents via ScoutStream.

use std::convert::Infallible;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use carbide_api_core::{Api, LogLine};
use futures::stream::{self, StreamExt};
use tokio::sync::broadcast::error::RecvError;

use super::Base;

/// Hard cap on a single page, so a client can't request an unbounded slice
/// regardless of the configured default page size.
const MAX_PAGE_SIZE: usize = 5000;

#[derive(Template)]
#[template(path = "api_logs.html")]
struct LogsPage {}

impl Base for LogsPage {}

/// `GET /admin/logs` — the unified live log viewer hub.
pub(super) async fn page() -> Html<String> {
    Html(LogsPage {}.render().unwrap())
}

/// Handle `GET /admin/logs/{source}/stream`, which opens up
/// the Server-Sent Events stream of nico-api log lines.
pub(super) async fn stream(State(state): State<Arc<Api>>, Path(source): Path<String>) -> Response {
    if source != "api" {
        return (
            StatusCode::NOT_FOUND,
            format!("log source {source:?} is not available yet"),
        )
            .into_response();
    }

    let page_size = state.runtime_config.log_history.page_size;
    let log_stream = state.dynamic_settings.log_stream.clone();
    // Subscribe before snapshotting the backlog so no line slips through the gap
    // between the two.
    let rx = log_stream.subscribe();
    let backlog = log_stream.latest(page_size);

    let replay = stream::iter(
        backlog
            .into_iter()
            .map(|line| Ok::<_, Infallible>(line_event(line.as_ref()))),
    );

    let live = stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(line) => Some((Ok::<_, Infallible>(line_event(line.as_ref())), rx)),
            // A subscriber that fell behind the bounded channel: tell the viewer
            // how many lines it missed rather than dropping the connection.
            Err(RecvError::Lagged(skipped)) => {
                let ev = Event::default().event("lag").data(skipped.to_string());
                Some((Ok(ev), rx))
            }
            Err(RecvError::Closed) => None,
        }
    });

    Sse::new(replay.chain(live))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Query parameters for the scrollback history endpoint.
#[derive(serde::Deserialize)]
pub(super) struct HistoryQuery {
    /// Return lines older than this `seq` cursor. Absent = newest page.
    before: Option<u64>,
    /// Max lines to return; clamped to `MAX_PAGE_SIZE`. Absent = `DEFAULT_PAGE_SIZE`.
    limit: Option<usize>,
}

/// `GET /admin/logs/{source}/history?before=<seq>&limit=<n>` — one page of
/// buffered lines (oldest-first) for scrollback. With `before`, returns the
/// page just older than that cursor; without it, the newest page.
pub(super) async fn history(
    State(state): State<Arc<Api>>,
    Path(source): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Response {
    if source != "api" {
        return (
            StatusCode::NOT_FOUND,
            format!("log source {source:?} is not available yet"),
        )
            .into_response();
    }

    let limit = query
        .limit
        .unwrap_or(state.runtime_config.log_history.page_size)
        .min(MAX_PAGE_SIZE);
    let log_stream = &state.dynamic_settings.log_stream;
    let lines = match query.before {
        Some(before) => log_stream.history(before, limit),
        None => log_stream.latest(limit),
    };

    let body = serde_json::to_string(&lines).unwrap_or_else(|_| "[]".to_string());
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

/// Serialize a log line into an SSE data frame (one JSON object per event).
fn line_event(line: &LogLine) -> Event {
    Event::default().data(serde_json::to_string(line).unwrap_or_default())
}
