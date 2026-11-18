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

/// Wraps the handler in a `request` span shared by logfmt and OTLP export.
///
/// A `traceparent` header continues the caller's trace, otherwise the span starts
/// a new one. `ForgeTlsClient` sends this span's context on outbound gRPC calls.
pub(crate) async fn logger(
    ConnectInfo(socket_addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    // Links request logs using the same correlation ID format as api-core.
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

    // Set the parent before entering the span; started spans cannot change it.
    trace_propagation::set_span_parent_from_headers(&span, request.headers());

    // An absent header leaves its `Empty` placeholder unset, so logfmt omits it.
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::extract::{ConnectInfo, Request};
    use opentelemetry::trace::{TraceContextExt, TraceId, TracerProvider as _};
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
    use tower::ServiceExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    // Fixed IDs make an inherited trace easy to recognize.
    const INBOUND_TRACE: u128 = 0x42;
    const INBOUND_SPAN: u64 = 0x55;

    /// Returns the handler's trace ID: the context an outbound call would use.
    /// Returns `None` if the handler has no valid trace context.
    async fn served_trace_id(traceparent: Option<&str>) -> Option<TraceId> {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("nico-pxe-test")));
        let _guard = tracing::subscriber::set_default(subscriber);

        let observed: Arc<Mutex<Option<TraceId>>> = Arc::new(Mutex::new(None));
        let captured = observed.clone();
        let app = axum::Router::new()
            .route(
                "/api/v0/pxe/boot",
                axum::routing::get(move || async move {
                    let context = tracing::Span::current().context();
                    let span_context = context.span().span_context().clone();
                    if span_context.is_valid() {
                        *captured.lock().unwrap() = Some(span_context.trace_id());
                    }
                }),
            )
            .route_layer(axum::middleware::from_fn(super::logger));

        let mut builder = Request::builder().uri("/api/v0/pxe/boot");
        if let Some(traceparent) = traceparent {
            builder = builder.header("traceparent", traceparent);
        }
        let mut request = builder.body(Body::empty()).unwrap();
        // Supply the peer address normally added by the HTTP server.
        request
            .extensions_mut()
            .insert(ConnectInfo::<std::net::SocketAddr>(
                "10.0.0.1:4242".parse().unwrap(),
            ));

        let response = app.oneshot(request).await.unwrap();
        assert!(
            response.status().is_success(),
            "the request itself must still be served: {}",
            response.status()
        );

        *observed.lock().unwrap()
    }

    #[tokio::test]
    async fn request_span_continues_an_inbound_trace() {
        let trace_id = served_trace_id(Some(&format!(
            "00-{INBOUND_TRACE:032x}-{INBOUND_SPAN:016x}-01"
        )))
        .await;

        assert_eq!(
            trace_id,
            Some(TraceId::from(INBOUND_TRACE)),
            "a request carrying a traceparent must stay on the caller's trace"
        );
    }

    // Missing or malformed trace context must still allow a fresh trace.
    #[tokio::test]
    async fn request_span_roots_a_fresh_trace_without_usable_inbound_context() {
        for traceparent in [None, Some("not-a-traceparent")] {
            let trace_id = served_trace_id(traceparent).await;

            assert!(
                trace_id.is_some_and(|trace_id| trace_id != TraceId::from(INBOUND_TRACE)),
                "expected a fresh root trace for traceparent {traceparent:?}, got {trace_id:?}"
            );
        }
    }
}
