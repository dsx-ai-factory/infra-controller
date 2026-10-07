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
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use prometheus::Encoder;

use crate::common::AppState;

/// Serves both metric registries as one text exposition: the
/// `metrics-exporter-prometheus` recorder first (the `http_*` request
/// metrics, names and labels unchanged), then the OTel registry the
/// instrumentation framework's events record into (the `carbide_pxe_*`
/// counters). The two registries hold disjoint metric names.
async fn metrics(state: State<AppState>) -> ([(axum::http::HeaderName, &'static str); 1], String) {
    // Make sure the metrics are fully updated prior to rendering them
    state.prometheus_handle.run_upkeep();

    let mut exposition = state.prometheus_handle.render();

    let mut buffer = Vec::new();
    match prometheus::TextEncoder::new().encode(&state.otel_registry.gather(), &mut buffer) {
        Ok(()) => exposition.push_str(&String::from_utf8_lossy(&buffer)),
        // Keep the scrape serveable: on an encode failure the OTel section
        // is absent from this response rather than corrupting it.
        Err(error) => tracing::error!(error = %error, "unable to encode the OTel metrics registry"),
    }

    // The official Prometheus exposition content type. The endpoint has
    // always served axum's plain-text default, which scrapers accept, but
    // the versioned form is what the format specifies.
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        exposition,
    )
}

pub(crate) fn get_router(path_prefix: &str) -> Router<AppState> {
    Router::new().route(path_prefix, get(metrics))
}

#[cfg(test)]
mod tests {
    use metrics_exporter_prometheus::PrometheusBuilder;

    use super::*;
    use crate::common::test_app_state;

    /// One scrape returns both registries' text: the forked axum layer's
    /// `http_*` metrics from the `metrics-exporter-prometheus` handle,
    /// followed by the `carbide_pxe_*` series from the OTel registry --
    /// encoded through the same exporter pipeline production uses.
    #[tokio::test]
    async fn metrics_route_renders_both_registries() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            metrics::counter!("http_requests_total", "endpoint" => "/api/v0/pxe/boot").increment(1);
        });

        let otel =
            metrics_endpoint::new_metrics_setup("carbide-pxe-metrics-route-test", "test", false)
                .expect("build a local OTel metrics setup");
        // The OTel prometheus exporter appends the counter suffix, exposing
        // this instrument as `carbide_pxe_boot_outcomes_total` -- the same
        // cancellation the instrumentation framework relies on.
        otel.meter
            .u64_counter("carbide_pxe_boot_outcomes")
            .build()
            .add(1, &[]);

        let mut state = test_app_state();
        state.prometheus_handle = handle;
        state.otel_registry = otel.registry.clone();

        let http_section = state.prometheus_handle.render();
        let (_content_type, exposition) = super::metrics(State(state)).await;

        assert!(
            exposition.starts_with(&http_section),
            "the frozen http_* exposition must render first, byte-identical, got:\n{exposition}"
        );
        assert!(
            exposition
                .lines()
                .any(|line| line.starts_with("http_requests_total")),
            "the http_* section must contain the seeded counter, got:\n{exposition}"
        );
        assert!(
            exposition
                .lines()
                .any(|line| line.starts_with("carbide_pxe_boot_outcomes_total")),
            "the OTel registry's series must render after it, got:\n{exposition}"
        );
    }
}
