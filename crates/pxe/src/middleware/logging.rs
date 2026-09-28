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
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument;

/// Emits one request log line through the fleet's logfmt subscriber.
///
/// Opens an `info` span named `request` around the downstream handler and
/// records the method, path, query, client address, and the request and
/// response headers of interest on it. The logfmt layer renders the span on
/// close as a `level=SPAN span_name=request ...` line, giving it the
/// `component` tag, level filtering, `span_id` correlation, and timing every
/// other span in the fleet gets.
pub(crate) async fn logger(
    ConnectInfo(socket_addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    // A per-request correlation id, matching the fleet's request logging
    // (api-core's `LogService`). The logfmt layer surfaces it as `span_id` on
    // the span line and on every child event, so a request and the logs emitted
    // while serving it share one key.
    let span_id = format!("{:#x}", u64::from_le_bytes(rand::random::<[u8; 8]>()));
    let span = tracing::info_span!(
        "request",
        span_id,
        remote_ip = %socket_addr.ip().to_canonical(),
        remote_port = socket_addr.port(),
        request_method = %request.method(),
        request_path = request.uri().path(),
        request_query = request.uri().query().unwrap_or_default(),
        request_headers_host = tracing::field::Empty,
        "request_headers_content-length" = tracing::field::Empty,
        "request_headers_user-agent" = tracing::field::Empty,
        response_status = tracing::field::Empty,
        "response_headers_content-length" = tracing::field::Empty,
    );

    // Header fields are only surfaced when present, matching what the request
    // actually carried; an absent header leaves its `Empty` placeholder unset,
    // so the logfmt layer omits it.
    if let Some(host) = request.headers().get("Host").and_then(|h| h.to_str().ok()) {
        span.record("request_headers_host", host);
    }
    if let Some(content_length) = request
        .headers()
        .get("Content-Length")
        .and_then(|h| h.to_str().ok())
    {
        span.record("request_headers_content-length", content_length);
    }
    if let Some(user_agent) = request
        .headers()
        .get("User-Agent")
        .and_then(|h| h.to_str().ok())
    {
        span.record("request_headers_user-agent", user_agent);
    }

    let response = next.run(request).instrument(span.clone()).await;

    span.record("response_status", response.status().as_str());
    if let Some(content_length) = response
        .headers()
        .get("Content-Length")
        .and_then(|h| h.to_str().ok())
    {
        span.record("response_headers_content-length", content_length);
    }

    response
}
