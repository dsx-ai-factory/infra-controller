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

//! W3C Trace Context propagation for HTTP and gRPC service boundaries.
//!
//! In a distributed system one request hops through many services; to see it as a single end-to-end
//! trace, each service must carry the W3C `traceparent`/`tracestate` headers across its network
//! edges — read them off incoming requests, write them onto outgoing ones. This crate is the thin
//! glue for that, on top of the standard OpenTelemetry propagator (installed once at startup) and
//! [`opentelemetry_http`]'s header adapters; there is no hand-rolled `traceparent` parsing, and one
//! code path serves both protocols (each exposes its headers as an `http::HeaderMap`).
//!
//! - **Ingress** — call [`set_span_parent_from_headers`] on the inbound request; it makes the
//!   caller's span the parent of the local request span, so this service's spans join that trace.
//! - **Egress** — by client type:
//!   - **tower clients** (tonic gRPC `Channel` or hyper client) — wrap in [`TraceInjectService`] to
//!     stamp every request automatically.
//!   - **non-tower clients** (`reqwest`):
//!     - normally, use the off-the-shelf `reqwest-tracing` middleware (a separate crate).
//!     - if you only hold the already-built request (so it can't go through a middleware client),
//!       call [`inject_current_context`] on its header map.
//! - **Proxying** — a service that copies inbound headers onto an outbound request has to drop the
//!   propagator's own headers first, or the upstream parents under the caller instead of the proxy;
//!   [`is_propagated_header`] identifies them without assuming which propagator is installed.
//!
//! [`extract_context`] (read) and [`inject_context`] (write) are the underlying primitives,
//! exposed for direct use when the helpers above don't fit.
//!
//! When no propagator has been installed, OpenTelemetry's default is a no-op propagator, so every
//! helper here is a no-op — safe to wire in unconditionally.

use std::task::{Context as TaskContext, Poll};

use opentelemetry::trace::TraceContextExt;
use opentelemetry::{Context, global};
use opentelemetry_http::{HeaderExtractor, HeaderInjector};
use tower::Service;
use tracing_opentelemetry::OpenTelemetrySpanExt;

// --- Ingress ---

/// Extract a remote [`Context`] from inbound request headers using the globally configured
/// text-map propagator. When the headers carry no valid trace context, there is nothing to copy
/// and the returned context has no valid span.
#[must_use = "the extracted context has no effect unless it is used (e.g. as a span parent)"]
pub fn extract_context(headers: &http::HeaderMap) -> Context {
    global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(headers)))
}

/// Extract the inbound trace context from `headers` and, when it carries a valid span, make it the
/// parent of `span`. No-op when the headers carry no valid inbound context: an absent or malformed
/// `traceparent` leaves `span`'s parent unchanged.
pub fn set_span_parent_from_headers(span: &tracing::Span, headers: &http::HeaderMap) {
    let parent_cx = extract_context(headers);
    if parent_cx.span().span_context().is_valid() {
        // `set_parent` is best-effort and returns a Result: it can fail when no OpenTelemetry
        // layer is active, when the span has already been started, or when the span is filtered
        // out by a tracing filter. In each case there is nothing to link the remote context to,
        // so the error is intentionally ignored.
        let _ = span.set_parent(parent_cx);
    }
}

// --- Proxying ---

/// Whether `name` is a header owned by the globally configured text-map propagator.
///
/// A proxy that copies inbound headers onto its upstream request has to skip these: egress injects
/// the local hop's context, and a copied-through header would either be overwritten or survive and
/// parent the upstream under the original caller instead of the proxy. Asking the propagator for its
/// own field set keeps that filter correct for whichever propagator is installed, rather than
/// assuming W3C's `traceparent`/`tracestate`.
///
/// Always false when no propagator is installed, since the default no-op propagator claims no
/// headers.
pub fn is_propagated_header(name: &str) -> bool {
    global::get_text_map_propagator(|propagator| {
        propagator
            .fields()
            .any(|field| field.eq_ignore_ascii_case(name))
    })
}

// --- Egress ---

/// Inject `cx` into outbound request headers using the globally configured text-map propagator.
/// When `cx` has no valid span context, there is no trace context to copy.
pub fn inject_context(cx: &Context, headers: &mut http::HeaderMap) {
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(cx, &mut HeaderInjector(headers));
    });
}

/// Inject the *current* tracing span's trace context into outbound request headers.
///
/// Reads the OpenTelemetry context attached to [`tracing::Span::current`]; when there is no active
/// span / valid context this writes nothing, leaving the request unchanged.
pub fn inject_current_context(headers: &mut http::HeaderMap) {
    inject_context(&tracing::Span::current().context(), headers);
}

/// A [`tower::Service`] that injects the current trace context into each outbound request's headers,
/// then defers to the inner service unchanged.
///
/// Wrap an HTTP/gRPC client service (hyper client, tonic `Channel`, …) with this so each request
/// carries `traceparent`/`tracestate` derived from the span that issued it.
#[derive(Clone, Debug)]
pub struct TraceInjectService<S> {
    inner: S,
}

impl<S> TraceInjectService<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S, B> Service<http::Request<B>> for TraceInjectService<S>
where
    S: Service<http::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: http::Request<B>) -> Self::Future {
        inject_current_context(req.headers_mut());
        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState, TracerProvider,
    };
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tower::ServiceExt;
    use tracing_subscriber::prelude::*;

    use super::*;

    fn install_w3c_propagator() {
        global::set_text_map_propagator(TraceContextPropagator::new());
    }

    /// Run `f` with a tracing subscriber that has an OpenTelemetry layer installed, so spans
    /// created inside it carry a real OTel context (what egress injection reads from).
    fn with_otel_subscriber<R>(f: impl FnOnce() -> R) -> R {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("trace-propagation-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        tracing::subscriber::with_default(subscriber, f)
    }

    fn context_with_span(trace: u128, span: u64, sampled: bool) -> Context {
        let sc = SpanContext::new(
            TraceId::from(trace),
            SpanId::from(span),
            if sampled {
                TraceFlags::SAMPLED
            } else {
                TraceFlags::default()
            },
            true,
            TraceState::default(),
        );
        Context::new().with_remote_span_context(sc)
    }

    // --- Ingress ---

    #[test]
    fn extract_returns_invalid_context_without_headers() {
        install_w3c_propagator();
        let cx = extract_context(&http::HeaderMap::new());
        assert!(!cx.span().span_context().is_valid());
    }

    #[test]
    fn extract_ignores_malformed_traceparent() {
        install_w3c_propagator();
        let mut headers = http::HeaderMap::new();
        headers.insert("traceparent", "not-a-valid-traceparent".parse().unwrap());
        let cx = extract_context(&headers);
        assert!(!cx.span().span_context().is_valid());
    }

    #[test]
    fn set_span_parent_links_span_to_inbound_trace() {
        install_w3c_propagator();
        let trace = 0x42u128;
        let mut headers = http::HeaderMap::new();
        inject_context(&context_with_span(trace, 0x55, true), &mut headers);

        with_otel_subscriber(|| {
            let span = tracing::info_span!("request");
            set_span_parent_from_headers(&span, &headers);
            // The local span now belongs to the inbound trace.
            assert_eq!(
                span.context().span().span_context().trace_id(),
                TraceId::from(trace)
            );
        });
    }

    #[test]
    fn set_span_parent_is_noop_without_inbound_trace() {
        install_w3c_propagator();
        let inbound = 0x42u128;
        with_otel_subscriber(|| {
            let span = tracing::info_span!("request");
            // No inbound headers: must not adopt any specific upstream trace.
            set_span_parent_from_headers(&span, &http::HeaderMap::new());
            assert_ne!(
                span.context().span().span_context().trace_id(),
                TraceId::from(inbound)
            );
        });
    }

    // --- Proxying ---

    #[test]
    fn propagated_headers_are_the_installed_propagator_fields() {
        install_w3c_propagator();

        // The W3C propagator's own two headers. Header names arrive in whatever case the caller
        // sent, while the propagator advertises them lowercase, so matching ignores case.
        assert!(is_propagated_header("traceparent"));
        assert!(is_propagated_header("tracestate"));
        assert!(is_propagated_header("TraceParent"));

        // Everything else is the proxied request's own payload and has to keep flowing.
        assert!(!is_propagated_header("content-type"));
        assert!(!is_propagated_header("x-request-id"));
        // A near-miss, to pin that this is an exact field match and not a prefix test.
        assert!(!is_propagated_header("trace"));
    }

    // --- Egress ---

    #[test]
    fn inject_then_extract_roundtrips() {
        install_w3c_propagator();
        let trace = 0x42u128;
        let span = 0x55u64;
        let cx = context_with_span(trace, span, true);

        let mut headers = http::HeaderMap::new();
        inject_context(&cx, &mut headers);

        // The standard W3C header is present...
        assert!(headers.contains_key("traceparent"));
        // ...and round-trips back to the same trace / span ids.
        let extracted = extract_context(&headers);
        let sc = extracted.span().span_context().clone();
        assert!(sc.is_valid());
        assert_eq!(sc.trace_id(), TraceId::from(trace));
        assert_eq!(sc.span_id(), SpanId::from(span));
        assert!(sc.is_sampled());
    }

    #[test]
    fn inject_writes_nothing_without_valid_context() {
        install_w3c_propagator();
        let mut headers = http::HeaderMap::new();
        inject_context(&Context::new(), &mut headers);
        assert!(!headers.contains_key("traceparent"));
    }

    #[test]
    fn inject_current_context_uses_active_span() {
        install_w3c_propagator();
        with_otel_subscriber(|| {
            let span = tracing::info_span!("egress");
            let _enter = span.enter();

            let mut headers = http::HeaderMap::new();
            inject_current_context(&mut headers);
            assert!(headers.contains_key("traceparent"));
        });
    }

    #[test]
    fn inject_current_context_writes_nothing_without_active_span() {
        install_w3c_propagator();
        // No OpenTelemetry layer / active span: egress must not emit a traceparent.
        let mut headers = http::HeaderMap::new();
        inject_current_context(&mut headers);
        assert!(!headers.contains_key("traceparent"));
    }

    #[tokio::test]
    async fn inject_layer_adds_traceparent_to_forwarded_request() {
        install_w3c_propagator();
        let inner = tower::service_fn(|req: http::Request<()>| async move {
            Ok::<_, std::convert::Infallible>(req)
        });
        let mut svc = TraceInjectService::new(inner);
        svc.ready().await.expect("inner service is always ready");

        // The layer injects synchronously in `call`, so the span need only be active there.
        let future = with_otel_subscriber(|| {
            let span = tracing::info_span!("egress");
            let _enter = span.enter();
            let req = http::Request::builder().body(()).unwrap();
            svc.call(req)
        });
        let forwarded = future.await.unwrap();
        assert!(forwarded.headers().contains_key("traceparent"));
    }

    // --- End-to-end ---

    /// End-to-end through a real OpenTelemetry pipeline with an in-memory span sink: an inbound
    /// `traceparent` (ingress) becomes the parent of the request span, and an outbound request made
    /// within that span (egress) carries the same trace onward. The exported span proves the link.
    /// Deterministic: `AlwaysOn` sampler + `SimpleSpanProcessor` (synchronous export on span end).
    #[test]
    fn ingress_then_egress_round_trips_through_a_recorded_span() {
        use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider};

        install_w3c_propagator();

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("trace-propagation-roundtrip");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));

        // A known trace arriving at the service's ingress boundary from an upstream caller.
        let inbound_trace = 0x42u128;
        let inbound_span = 0x55u64;
        let mut inbound_headers = http::HeaderMap::new();
        inject_context(
            &context_with_span(inbound_trace, inbound_span, true),
            &mut inbound_headers,
        );

        let mut egress_headers = http::HeaderMap::new();
        tracing::subscriber::with_default(subscriber, || {
            let request_span = tracing::info_span!("request");
            // INGRESS: adopt the inbound trace as the request span's parent.
            set_span_parent_from_headers(&request_span, &inbound_headers);
            let _entered = request_span.enter();
            // EGRESS: an outbound call issued within the request injects the current context.
            inject_current_context(&mut egress_headers);
            // request_span closes at end of scope -> exported synchronously by SimpleSpanProcessor.
        });

        // EGRESS: the outbound traceparent continues the inbound trace.
        assert_eq!(
            extract_context(&egress_headers)
                .span()
                .span_context()
                .trace_id(),
            TraceId::from(inbound_trace),
            "egress traceparent should carry the inbound trace id"
        );

        // INGRESS: the recorded request span belongs to the inbound trace, parented to the caller.
        let spans = exporter.get_finished_spans().expect("finished spans");
        let request = spans
            .iter()
            .find(|s| s.name == "request")
            .expect("request span should have been exported");
        assert_eq!(
            request.span_context.trace_id(),
            TraceId::from(inbound_trace)
        );
        assert_eq!(request.parent_span_id, SpanId::from(inbound_span));
    }

    /// `tracestate` (the W3C vendor list alongside `traceparent`) passes through ingress to egress
    /// unchanged — this crate adds no entry of its own.
    #[test]
    fn tracestate_passes_through_ingress_then_egress() {
        use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};

        install_w3c_propagator();

        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .build();
        let tracer = provider.tracer("trace-propagation-tracestate");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));

        // Inbound carries a traceparent plus a neutral, W3C-formatted two-entry tracestate.
        let inbound_trace = 0x42u128;
        let mut inbound_headers = http::HeaderMap::new();
        inject_context(
            &context_with_span(inbound_trace, 0x55, true),
            &mut inbound_headers,
        );
        inbound_headers.insert("tracestate", "key1=value1,key2=value2".parse().unwrap());

        let mut egress_headers = http::HeaderMap::new();
        tracing::subscriber::with_default(subscriber, || {
            let request_span = tracing::info_span!("request");
            set_span_parent_from_headers(&request_span, &inbound_headers);
            let _entered = request_span.enter();
            inject_current_context(&mut egress_headers);
        });

        let egress_tracestate = egress_headers
            .get("tracestate")
            .expect("egress should carry tracestate")
            .to_str()
            .unwrap();
        assert!(
            egress_tracestate.contains("key1=value1") && egress_tracestate.contains("key2=value2"),
            "egress tracestate should preserve both inbound entries, got: {egress_tracestate}"
        );
    }
}
