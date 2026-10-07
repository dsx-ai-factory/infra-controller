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

//! A logging middleware for carbide API server requests

use std::error::Error;
use std::sync::Arc;
use std::task::{Context, Poll};

use model::errors::OperatorError;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};
use tracing::Instrument;

use crate::errors::CarbideErrorWithLocation;

/// A tower Layer which creates a `LogService` for every request
#[derive(Debug, Clone)]
pub(crate) struct LogLayer {
    /// Captures metrics in server requests
    /// This is an `Arc` because it will be shared with every request handler
    metrics: Arc<RequestMetrics>,
}

impl LogLayer {
    pub(crate) fn new(meter: Meter) -> Self {
        // The metrics here loosly follow
        // https://opentelemetry.io/docs/reference/specification/metrics/semantic_conventions/http-metrics/#http-server
        // We include the service name here for extra discoverability,
        // and the unit since setting it on the metric does not seem to have any
        // impact on the prometheus export. On prometheus the metric shows up
        // unitless - which makes the user guess
        let request_times = meter
            .f64_histogram("carbide-api.grpc.server.duration")
            .with_description("Processing time for a request on the carbide API server")
            .with_unit("ms")
            .build();

        let db = sqlx_query_tracing::DatabaseMetricEmitters::new(&meter);

        let metrics = Arc::new(RequestMetrics {
            _meter: meter,
            request_times,
            db,
        });

        Self { metrics }
    }
}

impl<S> tower::Layer<S> for LogLayer {
    type Service = LogService<S>;

    fn layer(&self, service: S) -> Self::Service {
        LogService {
            service,
            metrics: self.metrics.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct RequestMetrics {
    _meter: Meter,
    request_times: Histogram<f64>,
    db: sqlx_query_tracing::DatabaseMetricEmitters,
}

// This service implements the Forge API server logging behavior
#[derive(Clone, Debug)]
pub(crate) struct LogService<S> {
    service: S,
    metrics: Arc<RequestMetrics>,
}

impl<S, RequestBody, ResponseBody> tower::Service<hyper::http::Request<RequestBody>>
    for LogService<S>
where
    S: tower::Service<
            hyper::http::Request<RequestBody>,
            Response = hyper::http::Response<ResponseBody>,
        > + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    RequestBody: tonic::codegen::Body + Send + 'static,
    ResponseBody: tonic::codegen::Body + Send + 'static,
{
    type Response = hyper::http::Response<ResponseBody>;
    type Error = S::Error;
    type Future = tonic::codegen::BoxFuture<Self::Response, S::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: hyper::http::Request<RequestBody>) -> Self::Future {
        let mut service = self.service.clone();
        let metrics = self.metrics.clone();
        let span_id = format!("{:#x}", u64::from_le_bytes(rand::random::<[u8; 8]>()));

        Box::pin(async move {
            // Try to extract connection information
            let mut client_address = std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
                std::net::Ipv4Addr::UNSPECIFIED,
                0,
            ));
            let mut client_certs = 0;
            if let Some(conn_attrs) = request
                .extensions()
                .get::<Arc<carbide_authn::middleware::ConnectionAttributes>>()
            {
                client_address = conn_attrs.peer_address;
                client_certs = conn_attrs.peer_certificates.len();
            }

            // Start a span which tracks the API request
            // Some information about the request is only known when the request finishes
            // or the payload has been deserialized.
            // For these `tracing::field::Empty` has to be used, so that the missing
            // information can be populated later.

            // Field names are taken from the crate opentelemetry_semantic_conventions, as of the
            // time this code was written. Some of the field names in the semantic conventions
            // standards have changed over time (ie. combining grpc_method and grpc_service into a
            // single field, renaming rpc.grpc.status_code to rpc.response.status_code), but we're
            // opting to keep using the original fields. So we don't use the opentelemetry-defined
            // constants (e.g. `opentelemetry_semantic_conventions::trace::HTTP_STATUS_CODE`), since
            // some of them have become deprecated and will be removed.
            let request_span = tracing::span!(
                parent: None,
                tracing::Level::INFO,
                "request",
                span_id,
                carbide.trace_root = true,
                http.url = %request.uri(),
                http.response.status_code = tracing::field::Empty,
                request = tracing::field::Empty,
                otel.status_code = tracing::field::Empty,
                otel.status_message = tracing::field::Empty,
                rpc.method = tracing::field::Empty,
                rpc.service = tracing::field::Empty,
                rpc.grpc.status_code = tracing::field::Empty,
                rpc.grpc.status_description = tracing::field::Empty,
                forge.machine_id = tracing::field::Empty,
                client.address = client_address.ip().to_canonical().to_string(),
                client.port = client_address.port() as u64,
                client.num_certs = client_certs as u64,
                logfmt.suppress = tracing::field::Empty,
                sql_queries = 0,
                sql_total_rows_affected = 0,
                sql_total_rows_returned = 0,
                sql_max_query_duration_us = 0,
                sql_max_query_duration_summary = tracing::field::Empty,
                sql_total_query_duration_us = 0,
                user.id = tracing::field::Empty, // Populated by auth layer
                tenant.organization_id = tracing::field::Empty,
                nico.error = tracing::field::Empty,
                nico.error_code = tracing::field::Empty,
                nico.mitigation = tracing::field::Empty,
                nico.error_location = tracing::field::Empty,
            );

            // Continue any distributed trace the caller started: make the inbound W3C trace
            // context (`traceparent`/`tracestate`) the parent of this request span. The shared
            // tower layer serves both the gRPC and REST listeners, so this single extractor
            // covers both ingress paths. No-op when there is no valid inbound context, leaving
            // the request span a fresh root (issue #2438).
            trace_propagation::set_span_parent_from_headers(&request_span, request.headers());

            // Try to extract the gRPC service and method from the URI
            let mut grpc_method: Option<String> = None;
            let mut grpc_service: Option<String> = None;
            if let Some(path) = request.uri().path_and_query()
                && *request.method() == hyper::http::Method::POST
                && path.query().is_none()
            {
                let parts: Vec<&str> = path.path().split('/').collect();
                if parts.len() == 3 {
                    // the path starts with an empty segment, and the middle
                    // segment is the service name, the last segment is the
                    // method
                    grpc_service = Some(parts[1].to_string());
                    grpc_method = Some(parts[2].to_string());
                }
            }

            if let Some(service) = &grpc_service {
                request_span.record("rpc.service", service);
            }
            if let Some(method) = &grpc_method {
                request_span.record("rpc.method", method);
            }

            let start = std::time::Instant::now();

            let result = service.call(request).instrument(request_span.clone()).await;

            let db_query_metrics = {
                let _e: tracing::span::Entered<'_> = request_span.enter();
                sqlx_query_tracing::fetch_and_update_current_span_attributes()
            };

            let elapsed = start.elapsed();

            // Holds the overall outcome of the request as a single log message
            let outcome: Result<(), String>;
            let http_code: Option<http::StatusCode>;
            let grpc_status: Option<tonic::Code>;
            let carbide_error: Option<&CarbideErrorWithLocation>;

            match &result {
                Ok(result) => {
                    http_code = Some(result.status());
                    request_span.record("http.response.status_code", result.status().as_u16());

                    if result.status() == hyper::http::StatusCode::OK {
                        // In gRPC the actual message status is not in the http status code,
                        // but actually in a header/trailer. Tonic stores the tonic::Status
                        // object in the HTTP response extensions, so we use that.
                        let tonic_status = result.extensions().get::<tonic::Status>();
                        carbide_error = tonic_status
                            .and_then(|s| s.source())
                            .and_then(|source| source.downcast_ref::<CarbideErrorWithLocation>());
                        let code = tonic_status.map(|s| s.code()).unwrap_or(tonic::Code::Ok);
                        grpc_status = Some(code);

                        let message = tonic_status
                            .as_ref()
                            .map(|s| s.message().to_owned())
                            .unwrap_or_else(String::new);

                        request_span.record("rpc.grpc.status_code", code as u64);
                        request_span.record(
                            "rpc.grpc.status_description",
                            format!("Code: {}, Message: {}", code.description(), message),
                        );

                        outcome = if code == tonic::Code::Ok {
                            Ok(())
                        } else {
                            Err(format!(
                                "gRPC Error: {}. Message: {}",
                                code.description(),
                                message
                            ))
                        }
                    } else {
                        outcome = Err(format!("HTTP status: {}", result.status()));
                        grpc_status = None;
                        carbide_error = None;
                    }
                }
                Err(_) => {
                    http_code = None;
                    grpc_status = None;
                    carbide_error = None;
                    outcome = Err("HTTP execution error".to_string());
                }
            }

            // If the tonic::Status came from a CarbideError, log interesting fields from it like
            // the nico-internal error code and mitigation.
            if let Some(CarbideErrorWithLocation { error, location }) = carbide_error {
                let schema = error.operator_error_schema();
                // Note: don't add schema.text here, it's always identical to what we already add to e.g. rpc.grpc.status_description
                request_span.record("nico.error_code", schema.error_code.to_string());
                if let Some(mitigation) = schema.mitigation {
                    request_span.record("nico.mitigation", mitigation);
                }

                // This represents the source location where the error was converted to a tonic::Status.
                request_span.record("nico.error_location", location.to_string());
            }

            request_span.record(
                "otel.status_code",
                if outcome.is_ok() { "ok" } else { "error" },
            );
            if let Err(e) = outcome {
                // Writing this field will set the span status to error
                // Therefore we only write it on errors
                request_span.record("otel.status_message", e);
            }

            // Fetch the opentelemetry context from the tracing span which
            // creates a `Context`
            {
                let _entered = request_span.enter();

                // The attributes follow
                // https://opentelemetry.io/docs/reference/specification/metrics/semantic_conventions/http-metrics/#attributes
                let mut attributes = vec![
                    KeyValue::new(
                        "grpc.method",
                        grpc_method.unwrap_or_else(|| "unknown".to_string()),
                    ),
                    KeyValue::new(
                        "http.status_code",
                        http_code
                            .map(|status| status.as_str().to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                    ),
                    KeyValue::new(
                        "grpc.status_code",
                        grpc_status
                            .map(|code| {
                                // Debug format is required
                                // Using code.to_string() will not yield the expected result
                                format!("{code:?}")
                            })
                            .unwrap_or_else(|| "Unknown".to_string()),
                    ),
                ];

                metrics
                    .request_times
                    .record(elapsed.as_secs_f64() * 1000.0, &attributes);

                // We use an attribute to distinguish the query counter from the
                // ones that are used for state controller operations
                attributes.push(KeyValue::new("operation", "grpc"));
                metrics.db.emit(&db_query_metrics, &attributes);
            }

            result
        })
    }
}
