/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::time::Duration;

use hyper::{HeaderMap, Request, Response};
use tonic::body::Body;
use tower::util::BoxCloneService;
use tower::{Layer, Service, ServiceExt};
use tower_http::classify::GrpcFailureClass;
use tower_http::trace::TraceLayer;
use tracing::Span;

use crate::forge_tls_client::TransportError;

/// Trace each HTTP/2 request, including connection probes, under its own client
/// span. TraceLayer keeps it alive through response trailers, so a gRPC failure
/// after HTTP 200 is still reported as an error. Connection tasks stay detached
/// by ForgeTlsClient's executor.
pub(crate) fn traced_transport<S, B>(
    service: S,
    timeout: Duration,
) -> BoxCloneService<Request<Body>, Response<Body>, TransportError>
where
    S: Service<Request<Body>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: Into<tower::BoxError>,
    B: hyper::body::Body<Data = hyper::body::Bytes> + Send + 'static,
    B::Error: Into<tower::BoxError> + std::fmt::Display,
{
    let service = trace_propagation::TraceInjectService::new(service);
    let service = tower::timeout::Timeout::new(service, timeout);
    TraceLayer::new_for_grpc()
        .make_span_with(|request: &Request<Body>| {
            let (service, method) = request
                .uri()
                .path()
                .trim_start_matches('/')
                .split_once('/')
                .unwrap_or(("", ""));
            tracing::info_span!(
                "grpc_client_request",
                otel.kind = "client",
                rpc.system = "grpc",
                rpc.service = service,
                rpc.method = method,
                rpc.grpc.status_code = tracing::field::Empty,
                http.response.status_code = tracing::field::Empty,
                otel.status_code = tracing::field::Empty,
                logfmt.suppress = true,
            )
        })
        .on_request(())
        .on_response(|response: &Response<B>, _: Duration, span: &Span| {
            span.record("http.response.status_code", response.status().as_u16());
            // Leave success unset: OpenTelemetry's explicit OK status cannot
            // be overridden by an error arriving later in gRPC trailers.
            if !response.status().is_success() {
                span.record("otel.status_code", "error");
            }
            record_grpc_status(response.headers(), span);
        })
        .on_body_chunk(())
        .on_eos(|trailers: Option<&HeaderMap>, _: Duration, span: &Span| {
            if let Some(trailers) = trailers {
                record_grpc_status(trailers, span);
            }
        })
        .on_failure(|_: GrpcFailureClass, _: Duration, span: &Span| {
            span.record("otel.status_code", "error");
        })
        .layer(service)
        .map_response(|response| response.map(Body::new))
        .map_err(TransportError::from)
        .boxed_clone()
}

fn record_grpc_status(metadata: &HeaderMap, span: &Span) {
    if let Some(code) = metadata
        .get("grpc-status")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u32>().ok())
    {
        span.record("rpc.grpc.status_code", code);
        if code != 0 {
            span.record("otel.status_code", "error");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use http_body_util::{BodyExt, StreamBody};
    use hyper::body::Frame;
    use opentelemetry::trace::{SpanKind, Status, TracerProvider};
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing::Instrument;
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    #[derive(Clone, Copy)]
    enum Reply {
        Headers(u32),
        Trailers(u32),
        TransportError,
    }

    #[tokio::test]
    async fn client_spans_pair_with_servers_and_record_grpc_outcomes() {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
        for (scenario, reply, expected_status, expected_code) in [
            (
                "success trailers",
                Reply::Trailers(0),
                Status::Unset,
                Some(0),
            ),
            (
                "error headers",
                Reply::Headers(7),
                Status::error(""),
                Some(7),
            ),
            (
                "error trailers after HTTP 200",
                Reply::Trailers(14),
                Status::error(""),
                Some(14),
            ),
            (
                "transport failure",
                Reply::TransportError,
                Status::error(""),
                None,
            ),
        ] {
            let exporter = InMemorySpanExporter::default();
            let provider = SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build();
            let subscriber = tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("grpc-test")));
            let service = tower::service_fn(move |request: Request<Body>| async move {
                if matches!(reply, Reply::TransportError) {
                    return Err::<Response<Body>, tower::BoxError>(
                        std::io::Error::other("transport failed").into(),
                    );
                }
                let server = tracing::info_span!(parent: None, "api_request", otel.kind = "server");
                trace_propagation::set_span_parent_from_headers(&server, request.headers());
                drop(server);
                let mut metadata = HeaderMap::new();
                let code = match reply {
                    Reply::Headers(code) | Reply::Trailers(code) => code,
                    _ => unreachable!(),
                };
                metadata.insert("grpc-status", code.to_string().parse().unwrap());
                let response = match reply {
                    Reply::Headers(_) => {
                        let mut response = Response::new(Body::empty());
                        *response.headers_mut() = metadata;
                        response
                    }
                    Reply::Trailers(_) => {
                        Response::new(Body::new(StreamBody::new(tokio_stream::iter([Ok::<
                            _,
                            Infallible,
                        >(
                            Frame::trailers(metadata),
                        )]))))
                    }
                    Reply::TransportError => unreachable!(),
                };
                Ok(response)
            });
            let transport = traced_transport(service, Duration::from_secs(1));
            let response = async {
                let caller = tracing::info_span!("proxy_request", otel.kind = "server");
                async {
                    let response = transport
                        .oneshot(
                            Request::builder()
                                .uri("http://api/forge.Forge/Version")
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await?;
                    response.into_body().collect().await.unwrap();
                    Ok::<_, TransportError>(())
                }
                .instrument(caller)
                .await
            }
            .with_subscriber(subscriber)
            .await;
            assert_eq!(
                response.is_err(),
                matches!(reply, Reply::TransportError),
                "{scenario}"
            );
            let spans = exporter.get_finished_spans().unwrap();
            let client = spans
                .iter()
                .find(|s| s.name == "grpc_client_request")
                .expect(scenario);
            let caller = spans
                .iter()
                .find(|s| s.name == "proxy_request")
                .expect(scenario);
            assert_eq!(client.span_kind, SpanKind::Client, "{scenario}");
            assert_eq!(
                client.parent_span_id,
                caller.span_context.span_id(),
                "{scenario}"
            );
            assert_eq!(client.status, expected_status, "{scenario}");
            let code = client
                .attributes
                .iter()
                .find(|a| a.key.as_str() == "rpc.grpc.status_code")
                .map(|a| a.value.to_string());
            assert_eq!(
                code,
                expected_code.map(|code| code.to_string()),
                "{scenario}"
            );
            if !matches!(reply, Reply::TransportError) {
                let server = spans
                    .iter()
                    .find(|s| s.name == "api_request")
                    .expect(scenario);
                assert_eq!(
                    server.parent_span_id,
                    client.span_context.span_id(),
                    "{scenario}"
                );
                assert_eq!(
                    server.span_context.trace_id(),
                    client.span_context.trace_id(),
                    "{scenario}"
                );
            }
        }
    }
}
