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

use async_trait::async_trait;
use http::Extensions;
use reqwest::{Request, Response};
use reqwest_middleware::{Middleware, Next, Result};
use tracing::Instrument;

/// Runs the upstream transport detached from the proxied request's span.
///
/// hyper-util's `TokioExecutor` instruments connection driver tasks with the
/// span current when the connection is established. A pooled connection
/// outlives the request that opened it, so without this the `bmc_proxy_request`
/// span (and the `reqwest_tracing` span under it) would stay open until the
/// connection closes. Register this after `TracingMiddleware` so the exported
/// request span keeps its parent and only the transport sees the detached span.
pub(crate) struct SpanIsolationMiddleware;

impl SpanIsolationMiddleware {
    fn span() -> tracing::Span {
        tracing::info_span!(parent: None, "bmc_proxy_upstream_http", logfmt.suppress = true)
    }
}

#[async_trait]
impl Middleware for SpanIsolationMiddleware {
    async fn handle(
        &self,
        req: Request,
        extensions: &mut Extensions,
        next: Next<'_>,
    ) -> Result<Response> {
        next.run(req, extensions).instrument(Self::span()).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::registry::LookupSpan;

    use super::*;

    /// Records the span the transport would run under.
    struct CaptureCurrentSpan(Mutex<Option<tracing::Span>>);

    #[async_trait]
    impl Middleware for CaptureCurrentSpan {
        async fn handle(
            &self,
            req: Request,
            extensions: &mut Extensions,
            next: Next<'_>,
        ) -> Result<Response> {
            *self.0.lock().unwrap() = Some(tracing::Span::current());
            next.run(req, extensions).await
        }
    }

    #[tokio::test]
    async fn transport_runs_under_a_root_span() {
        let registry = Arc::new(tracing_subscriber::registry());
        let captured = Arc::new(CaptureCurrentSpan(Mutex::new(None)));
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
            .with(SpanIsolationMiddleware)
            .with_arc(captured.clone())
            .build();

        let _guard = tracing::subscriber::set_default(registry.clone());
        let caller = tracing::info_span!("caller");
        // Nothing listens on port 1; the transport failure is irrelevant here.
        let _ = client
            .get("http://127.0.0.1:1/")
            .send()
            .instrument(caller.clone())
            .await;

        let transport_span = captured
            .0
            .lock()
            .unwrap()
            .take()
            .expect("transport span captured");
        let id = transport_span.id().expect("transport span is enabled");
        let span_ref = registry.span(&id).expect("transport span is registered");
        assert_eq!(span_ref.name(), "bmc_proxy_upstream_http");
        assert!(
            span_ref.parent().is_none(),
            "transport span must not descend from {:?}",
            caller.id()
        );
    }
}
