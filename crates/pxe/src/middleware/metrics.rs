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

// Completely forked from the `axum-metrics` crate, with PXE-specific adjustments.

//! Minimalist exporter-agnostic [`metrics`] instrumentation middleware for [`axum`].
//!
//! Note that we does not set [`metrics::Recorder`] backend automatically.
//! You must pick and install a `metrics` exporter like
//! [`metrics_exporter_prometheus`](https://crates.io/crates/metrics-exporter-prometheus)
//! to perform actualy statistics and export the result.
//!
//! ```
//! use axum::routing::get;
//! use metrics_exporter_prometheus::PrometheusBuilder;
//! use tokio::net::TcpListener;
//!
//! #[tokio::main]
//! async fn main() {
//! # } async fn test_main() {
//!     // Install a metrics exporter backend and expose metrics on 127.0.0.1:8050.
//!     // This also spawns a upkeeping thread for maintenance of histogram data.
//!     PrometheusBuilder::new()
//!         .with_http_listener(([127, 0, 0, 1], 8050))
//!         .install()
//!         .unwrap();
//!
//!     let router = axum::Router::new()
//!         .route("/", get(|| async { "hello world" }))
//!         // Instrument for metrics.
//!         .layer(axum_metrics::MetricLayer::default());
//!
//!     // Run the main server on 0.0.0.0:8000.
//!     let listener = TcpListener::bind("0.0.0.0:8000").await.unwrap();
//!     axum::serve(listener, router).await.unwrap();
//! }
//! ```
//!
//! ## Metrics
//!
//! The metric and label names are heavily inspired by
//! [caddy](https://caddyserver.com/docs/metrics#http-middleware-metrics).
//!
//! Following metrics are recorded:
//! - gauge `http_requests_in_flight`: the number of in-flight requests.
//!   Streaming responses are counted until the full stream completes.
//! - counter `http_requests_total`: the number of all requests ever processed.
//!   This is currently records after the response completes for simplicity.
//! - histogram `http_request_size_bytes`: body size of requests in bytes.
//!   If the request is only half-read, then the number of actual read bytes is recorded.
//! - histogram `http_response_size_bytes`: body size of requests in bytes.
//!   If the response is only half-written, then the number of actual written bytes is recorded.
//! - histogram `http_request_duration_seconds`: time of round-trip of request in seconds.
//!   This is measured from request received to the end of response body stream.
//! - histogram `http_response_duration_seconds`: time to first-byte of responss in seconds.
//!   This is measured from request received to the response future becomes ready.
//!
//! Following labels are attached to relavent metrics:
//! - `endpoint`: the matched route template ([`MatchedPath`]), or `unknown` otherwise.
//! - `method`: the request method.
//! - `code`: the response status code.
use std::borrow::Cow;
use std::future::Future;
use std::num::Saturating;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Instant;

use axum::extract::MatchedPath;
use axum::http::{Method, Request, Response, StatusCode};
use bytes::Buf;
use http_body::{Body, Frame, SizeHint};
use metrics::{Gauge, Label, counter, gauge, histogram};
use pin_project_lite::pin_project;
use tower_layer::Layer;
use tower_service::Service;

/// Layer for applying [`Metric`].
#[derive(Default, Clone)]
pub(crate) struct MetricLayer {
    _priv: (),
}

impl<S> Layer<S> for MetricLayer {
    type Service = Metric<S>;

    fn layer(&self, inner: S) -> Self::Service {
        Metric { inner }
    }
}

/// Middleware that instrument metrics on all requests and responses.
#[derive(Debug, Clone)]
pub(crate) struct Metric<S> {
    inner: S,
}

// impl<S> Metric<S> {
//     /// Get a reference to the underlying service.
//     #[must_use]
//     pub fn get_ref(&self) -> &S {
//         &self.inner
//     }
//
//     /// Get a mutable reference to the underlying service.
//     #[must_use]
//     pub fn get_mut(&mut self) -> &mut S {
//         &mut self.inner
//     }
//
//     /// Get the owned underlying service.
//     #[must_use]
//     pub fn into_inner(self) -> S {
//         self.inner
//     }
// }
//
impl<S, ReqBody, RespBody> Service<Request<ReqBody>> for Metric<S>
where
    S: Service<Request<RequestBody<ReqBody>>, Response = Response<RespBody>>,
    ReqBody: Body,
    RespBody: Body,
{
    type Response = Response<ResponseBody<RespBody>>;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let start_inst = Instant::now();
        let inflight_gauge = gauge!("http_requests_in_flight");

        let method = req.method().clone();
        let endpoint = match req.extensions().get::<MatchedPath>() {
            Some(path) => Cow::Owned(path.as_str().to_owned()),
            None => Cow::Borrowed("unknown"),
        };
        let mut labels = Vec::with_capacity(3);
        labels.extend([
            Label::new("endpoint", endpoint),
            Label::new("method", method_to_str(&method)),
        ]);

        let req = req.map(|inner| RequestBody {
            inner,
            metric: ReqMetric {
                body_size: Saturating(0),
                labels: labels.clone(),
            },
        });

        inflight_gauge.increment(1);
        let resp_metric = RespMetric {
            body_size: Saturating(0),
            labels,
            start_inst,
            inflight_gauge,
        };

        ResponseFuture {
            fut: self.inner.call(req),
            metric: Some(resp_metric),
        }
    }
}

#[repr(C)]
struct ReqMetric {
    body_size: Saturating<u64>,
    labels: Vec<Label>,
}

impl Drop for ReqMetric {
    fn drop(&mut self) {
        if self.body_size.0 != 0 {
            histogram!("http_request_size_bytes", self.labels.iter())
                .record(self.body_size.0 as f64);
        }
    }
}

pin_project! {
    /// Response body for [`Metric`].
    // Keep order.
    #[repr(C)]
    pub(crate) struct ResponseBody<B> {
        #[pin]
        inner: B,
        metric: RespMetric,
    }
}

impl<B: Body> Body for ResponseBody<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.as_mut().project();
        let ret = ready!(this.inner.poll_frame(cx));
        if let Some(Ok(frame)) = &ret
            && let Some(data) = frame.data_ref()
        {
            this.metric.body_size += data.remaining() as u64;
        }
        Poll::Ready(ret)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

pin_project! {
    /// Request body for [`Metric`].
    // Keep order.
    #[repr(C)]
    pub(crate) struct RequestBody<B> {
        #[pin]
        inner: B,
        metric: ReqMetric,
    }
}

impl<B: Body> Body for RequestBody<B> {
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.project();
        let ret = ready!(this.inner.poll_frame(cx));
        if let Some(Ok(frame)) = &ret
            && let Some(data) = frame.data_ref()
        {
            this.metric.body_size += data.remaining() as u64;
        }
        Poll::Ready(ret)
    }
}

// Keep order.
#[repr(C)]
struct RespMetric {
    body_size: Saturating<u64>,
    labels: Vec<Label>,
    start_inst: Instant,
    inflight_gauge: Gauge,
}

impl Drop for RespMetric {
    fn drop(&mut self) {
        self.inflight_gauge.decrement(1);

        let labels = self.labels.iter();
        counter!("http_requests_total", labels.clone()).increment(1);
        if self.body_size.0 != 0 {
            histogram!("http_response_size_bytes", labels.clone()).record(self.body_size.0 as f64);
        }
        histogram!("http_request_duration_seconds", labels).record(self.start_inst.elapsed());
    }
}

pin_project! {
    /// Response future for [`Metric`].
    // Keep order.
    #[repr(C)]
    pub(crate) struct ResponseFuture<Fut> {
        #[pin]
        fut: Fut,
        metric: Option<RespMetric>,
    }
}

impl<Fut, E, RespBody> Future for ResponseFuture<Fut>
where
    Fut: Future<Output = Result<Response<RespBody>, E>>,
    RespBody: Body,
{
    type Output = Result<Response<ResponseBody<RespBody>>, E>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().project();
        let resp = ready!(this.fut.poll(cx))?;
        let mut metric = this.metric.take().expect("poll after ready");
        metric
            .labels
            .push(Label::new("code", code_to_str(resp.status())));
        histogram!("http_response_duration_seconds", metric.labels.iter())
            .record(metric.start_inst.elapsed());

        Poll::Ready(Ok(resp.map(|inner| ResponseBody { inner, metric })))
    }
}

fn method_to_str(method: &Method) -> Cow<'static, str> {
    macro_rules! match_method {
        ($($tt:tt),*) => {
            match method {
                $(&Method::$tt => Cow::Borrowed(stringify!($tt)),)*
                m => Cow::Owned(m.to_string()),
            }
        };
    }
    match_method!(OPTIONS, GET, POST, PUT, DELETE, HEAD, TRACE, CONNECT, PATCH)
}

// WAIT: https://github.com/hyperium/http/pull/569
fn code_to_str(code: StatusCode) -> &'static str {
    const MIN: u16 = 100;
    const MAX: u16 = 999;
    const TABLE_LEN: usize = 3 * (MAX - MIN + 1) as usize;
    static TABLE: &str = {
        let table = &const {
            let mut buf = [0u8; TABLE_LEN];
            let mut code = MIN;
            let mut i = 0;
            while code <= MAX {
                buf[i] = b'0' + (code / 100) as u8;
                buf[i + 1] = b'0' + (code / 10 % 10) as u8;
                buf[i + 2] = b'0' + (code % 10) as u8;
                code += 1;
                i += 3;
            }
            buf
        };
        match std::str::from_utf8(table) {
            Ok(s) => s,
            Err(_) => unreachable!(),
        }
    };

    let code = code.as_u16();
    assert!((MIN..=MAX).contains(&code));
    let pos = 3 * (code - MIN) as usize;
    &TABLE[pos..pos + 3]
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use metrics_exporter_prometheus::PrometheusBuilder;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn missing_matched_paths_use_bounded_unknown_endpoint() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _guard = metrics::set_default_local_recorder(&recorder);
        let app = Router::new()
            .route("/known", get(|| async { StatusCode::NO_CONTENT }))
            .nest_service(
                "/nested",
                Router::new().route("/{id}", get(|| async { StatusCode::NO_CONTENT })),
            )
            .layer(MetricLayer::default());

        for (uri, expected_status) in [
            ("/unmatched/user-controlled", StatusCode::NOT_FOUND),
            ("/nested/user-controlled", StatusCode::NO_CONTENT),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .body(Body::empty())
                        .expect("build request"),
                )
                .await
                .expect("serve request");
            assert_eq!(response.status(), expected_status);
        }

        let exposition = handle.render();
        for code in ["204", "404"] {
            assert!(
                exposition.lines().any(|line| {
                    line.starts_with("http_requests_total{")
                        && line.contains(&format!("code=\"{code}\""))
                        && line.contains("endpoint=\"unknown\"")
                }),
                "missing MatchedPath must use the bounded unknown endpoint label, got:\n{exposition}"
            );
        }
        assert!(
            !exposition.contains("user-controlled"),
            "raw request paths must not appear in metric labels, got:\n{exposition}"
        );
    }
}
