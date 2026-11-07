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

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use carbide_instrument::emit;
use eyre::WrapErr;
use hyper::{Request, Response};
use opentelemetry::KeyValue;
use opentelemetry_prometheus::ExporterBuilder;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_NAMESPACE};
use prometheus::Registry;
use tonic::service::AxumBody;
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;
use tracing::Span;

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "fmds_http_request_started",
    metric_name = "http_requests_total",
    metric_name_unchecked,
    component = "fmds",
    log = info,
    metric = counter,
    message = "started request",
    describe = "Number of HTTP requests made."
)]
struct FmdsHttpRequestStarted {
    #[context]
    method: String,
    #[context]
    request_path: String,
}

impl FmdsHttpRequestStarted {
    fn new(request: &Request<AxumBody>) -> Self {
        Self {
            method: request.method().to_string(),
            request_path: request.uri().path().to_string(),
        }
    }
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "fmds_http_response_generated",
    metric_name = "request_latency_milliseconds",
    metric_name_unchecked,
    component = "fmds",
    log = info,
    metric = histogram,
    message = "response generated",
    describe = "HTTP request latency"
)]
struct FmdsHttpResponseGenerated {
    #[context(value)]
    latency_milliseconds: f64,
    #[observation]
    latency: Duration,
}

impl FmdsHttpResponseGenerated {
    fn new(latency: Duration) -> Self {
        Self {
            latency_milliseconds: latency.as_secs_f64() * 1000.0,
            latency,
        }
    }
}

/// `HttpRequestMetrics` is the compatibility handle returned by `init()`.
/// Events resolve their instruments through the global meter provider now,
/// but callers can keep passing this value into the middleware unchanged.
pub struct HttpRequestMetrics {
    _private: (),
}

/// Registers the Prometheus reader and global meter provider used by FMDS.
pub fn init() -> eyre::Result<(Registry, HttpRequestMetrics)> {
    let prometheus_registry = Registry::new();
    let exporter = ExporterBuilder::default()
        .with_registry(prometheus_registry.clone())
        .without_scope_info()
        .without_target_info()
        .build()
        .wrap_err("could not build prometheus exporter")?;

    let resource_attributes = opentelemetry_sdk::Resource::builder()
        .with_attributes([
            KeyValue::new(SERVICE_NAME, "carbide-fmds"),
            KeyValue::new(SERVICE_NAMESPACE, "forge-system"),
        ])
        .build();

    let meter_provider = SdkMeterProvider::builder()
        .with_reader(exporter)
        .with_resource(resource_attributes)
        .build();

    opentelemetry::global::set_meter_provider(meter_provider);

    Ok((prometheus_registry, HttpRequestMetrics { _private: () }))
}

/// Adds the request-count and latency Events used by FMDS.
pub fn with_http_request_trace_layer(router: Router, _metrics: Arc<HttpRequestMetrics>) -> Router {
    let layer = TraceLayer::new_for_http()
        .make_span_with(|request: &Request<AxumBody>| {
            tracing::info_span!(
                "http-request",
                method = %request.method(),
                uri = %request.uri(),
            )
        })
        .on_request(move |request: &Request<AxumBody>, _span: &Span| {
            emit(FmdsHttpRequestStarted::new(request));
        })
        .on_response(
            move |_response: &Response<AxumBody>, latency: Duration, _span: &Span| {
                emit(FmdsHttpResponseGenerated::new(latency));
            },
        );

    router.layer(ServiceBuilder::new().layer(layer))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::get;
    use carbide_instrument::testing::{
        ApproxHistogramSum, CapturedFieldKind, MetricsCapture, capture_logs,
    };
    use carbide_test_support::{Check, check_values};
    use tower::ServiceExt;

    use super::*;

    const REQUEST_METRIC: &str = "http_requests_total";
    const LATENCY_METRIC: &str = "request_latency_milliseconds";

    enum EventCase {
        Request,
        Response,
    }

    #[derive(Debug, PartialEq)]
    struct EventObservation {
        request_delta: f64,
        latency_count_delta: u64,
        latency_sum_delta: ApproxHistogramSum,
        logs: Vec<LogObservation>,
    }

    #[derive(Debug, PartialEq)]
    struct LogObservation {
        metadata_name: String,
        level: tracing::Level,
        message: String,
        fields: Vec<(String, String)>,
        method_kind: Option<CapturedFieldKind>,
        request_path_kind: Option<CapturedFieldKind>,
        latency_kind: Option<CapturedFieldKind>,
    }

    fn observe_event(case: EventCase) -> EventObservation {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| match case {
            EventCase::Request => emit(FmdsHttpRequestStarted {
                method: "GET".to_string(),
                request_path: "/latest/meta-data".to_string(),
            }),
            EventCase::Response => emit(FmdsHttpResponseGenerated::new(Duration::from_micros(
                12_500,
            ))),
        })
        .into_iter()
        .map(|log| LogObservation {
            method_kind: log.field_kind("method"),
            request_path_kind: log.field_kind("request_path"),
            latency_kind: log.field_kind("latency_milliseconds"),
            metadata_name: log.metadata_name,
            level: log.level,
            message: log.message,
            fields: log.fields,
        })
        .collect();

        EventObservation {
            request_delta: metrics.counter_delta(REQUEST_METRIC, &[]),
            latency_count_delta: metrics.histogram_count_delta(LATENCY_METRIC, &[]),
            latency_sum_delta: metrics.histogram_sum_delta(LATENCY_METRIC, &[]),
            logs,
        }
    }

    #[test]
    fn http_events_preserve_metrics_and_structured_logs() {
        check_values(
            [
                Check {
                    scenario: "request start increments the legacy counter and logs request context",
                    input: EventCase::Request,
                    expect: EventObservation {
                        request_delta: 1.0,
                        latency_count_delta: 0,
                        latency_sum_delta: ApproxHistogramSum(0.0),
                        logs: vec![LogObservation {
                            metadata_name: "fmds_http_request_started".to_string(),
                            level: tracing::Level::INFO,
                            message: "started request".to_string(),
                            fields: vec![
                                (
                                    "event_name".to_string(),
                                    "fmds_http_request_started".to_string(),
                                ),
                                ("metric_name".to_string(), REQUEST_METRIC.to_string()),
                                ("method".to_string(), "GET".to_string()),
                                ("request_path".to_string(), "/latest/meta-data".to_string()),
                            ],
                            method_kind: Some(CapturedFieldKind::Debug),
                            request_path_kind: Some(CapturedFieldKind::Debug),
                            latency_kind: None,
                        }],
                    },
                },
                Check {
                    scenario: "response completion records milliseconds and logs native latency",
                    input: EventCase::Response,
                    expect: EventObservation {
                        request_delta: 0.0,
                        latency_count_delta: 1,
                        latency_sum_delta: ApproxHistogramSum(12.5),
                        logs: vec![LogObservation {
                            metadata_name: "fmds_http_response_generated".to_string(),
                            level: tracing::Level::INFO,
                            message: "response generated".to_string(),
                            fields: vec![
                                (
                                    "event_name".to_string(),
                                    "fmds_http_response_generated".to_string(),
                                ),
                                ("metric_name".to_string(), LATENCY_METRIC.to_string()),
                                ("latency_milliseconds".to_string(), "12.5".to_string()),
                            ],
                            method_kind: None,
                            request_path_kind: None,
                            latency_kind: Some(CapturedFieldKind::F64),
                        }],
                    },
                },
            ],
            observe_event,
        );
    }

    #[test]
    fn tracing_layer_emits_one_start_and_completion_event_per_request() {
        let metrics = MetricsCapture::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build");
        let logs = capture_logs(|| {
            runtime.block_on(async {
                let router = with_http_request_trace_layer(
                    Router::new().route("/health", get(|| async { StatusCode::NO_CONTENT })),
                    Arc::new(HttpRequestMetrics { _private: () }),
                );
                let response = router
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/health")
                            .body(Body::empty())
                            .expect("test request should build"),
                    )
                    .await
                    .expect("test request should complete");
                assert_eq!(response.status(), StatusCode::NO_CONTENT);
            });
        });

        assert_eq!(metrics.counter_delta(REQUEST_METRIC, &[]), 1.0);
        assert_eq!(metrics.histogram_count_delta(LATENCY_METRIC, &[]), 1);
        assert_eq!(
            logs.iter()
                .map(|log| log.metadata_name.as_str())
                .collect::<Vec<_>>(),
            ["fmds_http_request_started", "fmds_http_response_generated",]
        );
    }
}
