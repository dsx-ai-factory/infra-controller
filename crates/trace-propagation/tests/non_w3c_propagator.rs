/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Verifies [`trace_propagation::is_propagated_header`] reports whatever the *installed* propagator
//! advertises rather than a hardcoded W3C field list, so a proxy built on it keeps filtering
//! correctly if a deployment ever configures a different format (B3, Jaeger, a composite, ...).
//!
//! This is its own test binary (separate process) because it installs a non-W3C global propagator,
//! which would otherwise leak into every other test in the crate.

use opentelemetry::propagation::text_map_propagator::FieldIter;
use opentelemetry::propagation::{Extractor, Injector, TextMapPropagator};
use opentelemetry::{Context, global};
use trace_propagation::is_propagated_header;

/// Carries its context in one non-W3C header, standing in for any alternative format. Only
/// [`TextMapPropagator::fields`] matters here, so inject/extract are left inert.
#[derive(Debug)]
struct SingleHeaderPropagator {
    fields: Vec<String>,
}

impl SingleHeaderPropagator {
    const HEADER: &'static str = "x-trace-id";

    fn new() -> Self {
        Self {
            fields: vec![Self::HEADER.to_string()],
        }
    }
}

impl TextMapPropagator for SingleHeaderPropagator {
    fn inject_context(&self, _cx: &Context, _injector: &mut dyn Injector) {}

    fn extract_with_context(&self, cx: &Context, _extractor: &dyn Extractor) -> Context {
        cx.clone()
    }

    fn fields(&self) -> FieldIter<'_> {
        FieldIter::new(&self.fields)
    }
}

#[test]
fn propagated_headers_follow_a_non_w3c_propagator() {
    global::set_text_map_propagator(SingleHeaderPropagator::new());

    assert!(is_propagated_header(SingleHeaderPropagator::HEADER));

    // W3C's headers belong to no installed propagator here, so a proxy treats them as ordinary
    // request payload. Hardcoding `traceparent`/`tracestate` would get both of these wrong.
    assert!(!is_propagated_header("traceparent"));
    assert!(!is_propagated_header("tracestate"));
}
