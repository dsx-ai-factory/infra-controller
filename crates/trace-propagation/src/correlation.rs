// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Durable log correlation using the originating trace ID, without restoring spans.
//!
//! Persist the ID with queued work and restore it for each execution attempt.
//! Multiple operations originating in one trace intentionally share the ID.
//! Spawned tasks must explicitly restore their scope; Tokio task locals are not inherited.

use std::fmt;
use std::future::Future;
use std::str::FromStr;

use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::TraceContextExt;
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Header carrying a log correlation ID, independently of the active OTel trace.
pub const HEADER: &str = "x-nico-trace-id";

/// Nonzero 128-bit trace ID, serialized as exactly 32 lowercase hexadecimal digits.
/// Its presence does not imply that a trace was exported or that a span is still alive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TraceId(opentelemetry::TraceId);

impl TraceId {
    /// Generate a log-only correlation ID when no incoming trace is available.
    pub fn generate() -> Self {
        Self(opentelemetry::TraceId::from_bytes(
            *uuid::Uuid::new_v4().as_bytes(),
        ))
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for TraceId {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 32
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("trace ID must contain 32 lowercase hexadecimal digits");
        }
        let id = opentelemetry::TraceId::from_hex(value).map_err(|_| "invalid trace ID")?;
        if id == opentelemetry::TraceId::INVALID {
            return Err("trace ID must not be zero");
        }
        Ok(Self(id))
    }
}

impl TryFrom<String> for TraceId {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<TraceId> for String {
    fn from(value: TraceId) -> Self {
        value.to_string()
    }
}

tokio::task_local! {
    static LOG_TRACE_ID: TraceId;
}

/// Read the durable log ID, falling back to the current valid OTel context.
pub fn current_id() -> Option<TraceId> {
    LOG_TRACE_ID.try_with(|id| *id).ok().or_else(|| {
        let context = tracing::Span::current().context();
        let span = context.span();
        let context = span.span_context();
        context.is_valid().then(|| TraceId(context.trace_id()))
    })
}

/// Prefer an explicit log ID, then a valid W3C traceparent, then the current context.
/// Missing context generates a log-only ID. Malformed or repeated log-ID headers fail;
/// invalid traceparent is ignored according to the W3C propagator's behavior.
pub fn from_headers(headers: &http::HeaderMap) -> Result<TraceId, &'static str> {
    let mut values = headers.get_all(HEADER).iter();
    if let Some(value) = values.next() {
        if values.next().is_some() {
            return Err("x-nico-trace-id must occur once");
        }
        return value
            .to_str()
            .map_err(|_| "invalid x-nico-trace-id")?
            .parse();
    }
    let context = opentelemetry_sdk::propagation::TraceContextPropagator::new()
        .extract(&opentelemetry_http::HeaderExtractor(headers));
    let span = context.span();
    let context = span.span_context();
    Ok(if context.is_valid() {
        TraceId(context.trace_id())
    } else {
        current_id().unwrap_or_else(TraceId::generate)
    })
}

/// Attach the ID to a logging scope without restoring the originating OTel context.
pub async fn scope<F: Future>(trace_id: TraceId, future: F) -> F::Output {
    LOG_TRACE_ID
        .scope(
            trace_id,
            future.instrument(tracing::info_span!("cm_operation", %trace_id)),
        )
        .await
}

/// Restore persisted work; legacy work without an ID remains unchanged.
pub async fn resume<F: Future>(trace_id: Option<TraceId>, future: F) -> F::Output {
    match trace_id {
        Some(id) => scope(id, future).await,
        None => future.await,
    }
}

pub(super) fn inject_current(headers: &mut http::HeaderMap) {
    if let Ok(id) = LOG_TRACE_ID.try_with(|id| *id) {
        headers.insert(
            HEADER,
            http::HeaderValue::from_str(&id.to_string())
                .expect("a validated hexadecimal trace ID is an ASCII header value"),
        );
    }
}

#[cfg(test)]
mod tests {
    use tower::{Service, ServiceExt};

    use super::*;

    const ID: &str = "1234567890abcdef1234567890abcdef";

    #[tokio::test]
    async fn restored_log_id_does_not_replace_the_background_trace() {
        use opentelemetry::trace::TracerProvider;
        use tracing::instrument::WithSubscriber;
        use tracing_subscriber::prelude::*;

        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        async {
            let background = tracing::info_span!("background");
            let background_id = background.context().span().span_context().trace_id();
            let original: TraceId = ID.parse().unwrap();
            assert_ne!(original.0, background_id);
            async {
                assert_eq!(current_id().unwrap().0, background_id);
                resume(Some(original), async {
                    assert_eq!(current_id(), Some(original));
                    assert_eq!(
                        tracing::Span::current()
                            .context()
                            .span()
                            .span_context()
                            .trace_id(),
                        background_id
                    );
                    let mut headers = http::HeaderMap::new();
                    inject_current(&mut headers);
                    assert_eq!(headers[HEADER], ID);
                    assert!(!headers.contains_key("traceparent"));
                })
                .await;
                assert_eq!(current_id().unwrap().0, background_id);
            }
            .instrument(background)
            .await;
        }
        .with_subscriber(subscriber)
        .await;
    }

    #[test]
    fn trace_id_wire_and_persistence_contract() {
        for (value, valid) in [
            (ID, true),
            ("00000000000000000000000000000000", false),
            ("1234567890ABCDEF1234567890ABCDEF", false),
            ("12345678-90ab-cdef-1234-567890abcdef", false),
            ("short", false),
            ("1234567890abcdef1234567890abcdeg", false),
        ] {
            let parsed = value.parse::<TraceId>();
            assert_eq!(parsed.is_ok(), valid, "{value}");
            let json = serde_json::to_string(value).unwrap();
            assert_eq!(serde_json::from_str::<TraceId>(&json).is_ok(), valid);
            if let Ok(id) = parsed {
                assert_eq!(id.to_string(), value);
                assert_eq!(serde_json::to_string(&id).unwrap(), json);
            }
        }
    }

    #[test]
    fn ingress_reuses_traceparent_without_an_exporter() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "traceparent",
            format!("00-{ID}-1234567890abcdef-00").parse().unwrap(),
        );
        assert_eq!(from_headers(&headers).unwrap().to_string(), ID);

        let explicit = TraceId::generate();
        headers.insert(HEADER, explicit.to_string().parse().unwrap());
        assert_eq!(from_headers(&headers).unwrap(), explicit);
        headers.append(HEADER, explicit.to_string().parse().unwrap());
        assert!(from_headers(&headers).is_err());
        headers.remove(HEADER);
        headers.insert("traceparent", "invalid".parse().unwrap());
        let generated = from_headers(&headers).unwrap();
        assert_eq!(generated.to_string().parse::<TraceId>().unwrap(), generated);
    }

    #[tokio::test]
    async fn scopes_isolate_concurrent_work_and_restore_parents() {
        let check = |id| async move {
            scope(id, async {
                tokio::task::yield_now().await;
                assert_eq!(current_id(), Some(id));
                let child = TraceId::generate();
                scope(child, async {
                    assert_eq!(current_id(), Some(child));
                })
                .await;
                assert_eq!(current_id(), Some(id));
            })
            .await;
            assert_eq!(current_id(), None);
        };
        tokio::join!(check(TraceId::generate()), check(TraceId::generate()));
        resume(None, async {
            assert_eq!(current_id(), None);
        })
        .await;
    }

    #[tokio::test]
    async fn transport_preserves_traceparent_and_forwards_persisted_id() {
        let id: TraceId = serde_json::from_str(&format!("\"{ID}\"")).unwrap();
        let transport = tower::service_fn(|request: http::Request<()>| async move {
            Ok::<_, std::convert::Infallible>(request.into_parts().0.headers)
        });
        let mut client = crate::TraceInjectService::new(transport);
        let headers = resume(Some(id), async {
            let mut request = http::Request::new(());
            request
                .headers_mut()
                .insert(HEADER, "stale".parse().unwrap());
            // Without an active OTel span, this is not fabricated from the log ID.
            request
                .headers_mut()
                .insert("traceparent", "existing-context".parse().unwrap());
            client.ready().await.unwrap().call(request).await.unwrap()
        })
        .await;
        assert_eq!(headers[HEADER], ID);
        assert_eq!(headers["traceparent"], "existing-context");
        let headers = client
            .ready()
            .await
            .unwrap()
            .call(http::Request::new(()))
            .await
            .unwrap();
        assert!(!headers.contains_key(HEADER));
    }

    #[tokio::test]
    async fn cancellation_does_not_leak_correlation() {
        let id = TraceId::generate();
        let mut future = Box::pin(scope(id, async {
            assert_eq!(current_id(), Some(id));
            std::future::pending::<()>().await;
        }));
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(future);
        assert_eq!(current_id(), None);
    }
}
