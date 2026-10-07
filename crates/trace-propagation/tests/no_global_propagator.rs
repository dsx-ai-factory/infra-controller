/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Verifies the crate's helpers stay safe no-ops when no OpenTelemetry **global text-map
//! propagator** is installed — e.g. a component that uses this crate but never configures
//! tracing, or a process before its startup wiring runs.
//!
//! This is its own test binary (separate process), so — unlike the in-lib unit tests, which call
//! `set_text_map_propagator` — nothing installs a propagator here and the global stays at
//! OpenTelemetry's no-op default. The tests only *read* the global propagator, never set it, so they
//! are safe to run concurrently.
//!
//! **Do not add a test to this file that installs a propagator:** the global is process-wide, so it
//! would leak into the other tests. The [`assert_no_global_propagator`] guard at the top of each test
//! fails fast if that ever happens.

use opentelemetry::global;
use tower::{Service, ServiceExt};
use trace_propagation::{
    TraceInjectService, extract_context, inject_context, inject_current_context,
    is_propagated_header, set_span_parent_from_headers,
};

const SAMPLE_TRACEPARENT: &str = "00-1111111111111111aaaaaaaaaaaaaaaa-2222222222222222-01";

fn headers_with_traceparent() -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    headers.insert("traceparent", SAMPLE_TRACEPARENT.parse().unwrap());
    headers
}

/// Precondition for this file: no global text-map propagator is installed in this test binary. The
/// default no-op propagator advertises no fields, so any installed propagator reports at least one.
fn assert_no_global_propagator() {
    let has_fields = global::get_text_map_propagator(|p| p.fields().next().is_some());
    assert!(
        !has_fields,
        "a global text-map propagator is installed in this test binary; this file's tests require none",
    );
}

#[test]
fn ingress_and_egress_are_noops_without_a_global_propagator() {
    assert_no_global_propagator();

    // Ingress: parenting a span from real inbound headers must not panic, and (no propagator) must
    // not adopt a remote parent.
    let inbound = headers_with_traceparent();
    set_span_parent_from_headers(&tracing::info_span!("ingress"), &inbound); // must not panic

    // Egress: injecting writes nothing without an active trace / propagator.
    let mut outbound = http::HeaderMap::new();
    inject_context(&extract_context(&inbound), &mut outbound);
    inject_current_context(&mut outbound);
    assert!(
        outbound.is_empty(),
        "no trace/propagator -> nothing injected"
    );

    // ...and still no propagator, so the no-ops above genuinely ran under that condition throughout.
    assert_no_global_propagator();
}

#[test]
fn no_header_is_propagator_owned_without_a_global_propagator() {
    assert_no_global_propagator();

    // A process that propagates nothing owns no hop, so a proxy built on this has nothing to strip
    // and forwards trace headers as the opaque payload they are to it.
    assert!(!is_propagated_header("traceparent"));
    assert!(!is_propagated_header("tracestate"));

    assert_no_global_propagator();
}

#[tokio::test]
async fn trace_inject_service_forwards_unchanged_without_a_global_propagator() {
    assert_no_global_propagator();

    // Wrapping a brand-new client in TraceInjectService and driving a request through it must not
    // panic when there's no propagator/active span; it forwards the request untouched.
    let inner = tower::service_fn(|req: http::Request<()>| async move {
        Ok::<_, std::convert::Infallible>(req)
    });
    let mut svc = TraceInjectService::new(inner);
    let req = http::Request::builder().body(()).unwrap();
    let forwarded = svc.ready().await.unwrap().call(req).await.unwrap();
    assert!(
        !forwarded.headers().contains_key("traceparent"),
        "no trace -> no header injected"
    );

    // The egress no-op above can't itself detect an installed propagator (no active span), so
    // confirm the precondition still held for the whole test.
    assert_no_global_propagator();
}
