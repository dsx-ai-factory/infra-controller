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

use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use carbide_api_core::LogStream;
use carbide_api_core::bootstrap::{
    ActiveLevel, LogStreamLayer, Logging, ReloadableFilter, dep_log_filter,
};
use carbide_api_core::cfg::file::TracingConfig;
use eyre::WrapErr;
use opentelemetry::trace::{Link, SpanKind, TraceContextExt, TracerProvider};
use opentelemetry::{Context, KeyValue, TraceId, Value, global};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{SamplingDecision, SamplingResult, ShouldSample};
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, filter, reload};

/// The admin-UI log-stream layer, or `None` when the admin UI is disabled.
/// The layer's only consumer is the web UI's log viewer: with the UI off there
/// is no subscriber, so installing it would spend per-event work (a second
/// field visit, timestamp/target strings, ring-buffer churn under a mutex) on
/// lines nothing reads. Composed into the registry as an `Option`, which
/// `tracing-subscriber` treats as a no-op layer when `None`.
fn admin_ui_log_stream_layer(
    enable_admin_ui: bool,
    log_stream: &LogStream,
) -> Option<LogStreamLayer> {
    enable_admin_ui.then(|| LogStreamLayer::new(log_stream.clone()))
}

pub(super) fn setup_logging(
    debug: u8,
    extra_logfmt_event_fields: Vec<String>,
    override_logging_subscriber: Option<impl SubscriberInitExt>,
    log_history_max_bytes: usize,
    enable_admin_ui: bool,
    tracing_config: &TracingConfig,
) -> eyre::Result<Logging> {
    // Install the global W3C trace-context propagator so NICo can extract inbound and inject
    // outbound `traceparent`/`tracestate` at network boundaries (issue #2438). Without it,
    // OpenTelemetry's default propagator is a no-op.
    global::set_text_map_propagator(TraceContextPropagator::new());

    // This configures emission of logs in LogFmt syntax
    // and emission of metrics
    let log_level = match debug {
        0 => LevelFilter::INFO,
        1 => {
            // command line overrides config file
            LevelFilter::DEBUG
        }
        _ => LevelFilter::TRACE,
    };

    // We set up some global filtering using `tracing`s `EnvFilter` framework
    // The global filter will apply to all `Layer`s that are added to the
    // `logging_subscriber` later on. This means it applies for both logging to
    // stdout as well as for OpenTelemetry integration.
    // We ignore a lot of spans and events from 3rd party frameworks
    let initial_log_filter = EnvFilter::builder()
        .with_default_directive(log_level.into())
        .from_env()?;
    let initial_log_filter = dep_log_filter(initial_log_filter);

    let (logfmt_stdout_filter, logfmt_stdout_reload_handle) =
        reload::Layer::new(initial_log_filter.clone());
    let mut event_fields = vec![logfmt::EventField::with_default("component", "nico-api")];
    event_fields.extend(
        extra_logfmt_event_fields
            .into_iter()
            .map(logfmt::EventField::new),
    );
    let logfmt_stdout_formatter = logfmt::layer().with_event_fields(event_fields);
    let spancount_layer = spancounter::layer();
    let spancount_reader = spancount_layer.reader();
    let log_events = carbide_instrument::LogEventsMetric::new("nico-api");

    // Used as part of a layer for collecting + brodcasting
    // log events to the admin web UI.
    let log_stream = LogStream::with_max_bytes(log_history_max_bytes);

    // == Dynamic filter for tracing enabled/disabled ==
    // This doesn't track levels but instead just enabled/disabled (when we want tracing enabled, we
    // typically want a high level of verbosity.) Enabled by default if debug is enabled.
    let tracing_enabled = Arc::new(AtomicBool::new(debug == 1 || tracing_config.enabled));
    let trace_sampler = CarbideSpanSampler::new(tracing_enabled.clone());
    let trace_filter =
        filter::filter_fn(should_accept_span_or_event).with_max_level_hint(log_level);

    if let Some(logging_subscriber) = override_logging_subscriber {
        logging_subscriber
            .try_init()
            .wrap_err("logging_subscriber.try_init()")?;
    } else {
        let maybe_otel_tracing_layer =
            match std::env::var(opentelemetry_otlp::OTEL_EXPORTER_OTLP_TRACES_ENDPOINT)
                .ok()
                .or_else(|| tracing_config.otlp_endpoint.clone())
            {
                None => None,
                Some(endpoint) => {
                    let otlp_exporter = build_span_exporter(&endpoint)?;

                    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                        // CarbideSpanSampler selects explicitly marked application trace roots.
                        .with_sampler(trace_sampler)
                        .with_batch_exporter(otlp_exporter)
                        .with_resource(
                            Resource::builder()
                                .with_attributes([KeyValue::new("service.name", "carbide-api")])
                                .build(),
                        )
                        .build();
                    Some(
                        tracing_opentelemetry::layer()
                            .with_tracer(tracer_provider.tracer("carbide"))
                            .with_filter(trace_filter),
                    )
                }
            };

        // Installed only when the admin UI can consume it; `None` keeps this
        // second per-event consumer out of the subscriber stack entirely.
        let log_stream_layer = admin_ui_log_stream_layer(enable_admin_ui, &log_stream)
            .map(|layer| layer.with_filter(initial_log_filter.clone()));

        tracing_subscriber::registry()
            .with(log_events.layer().with_filter(initial_log_filter.clone()))
            .with(spancount_layer.with_filter(log_level))
            .with(maybe_otel_tracing_layer)
            .with(logfmt_stdout_formatter.with_filter(logfmt_stdout_filter))
            .with(log_stream_layer)
            .with(sqlx_query_tracing::create_sqlx_query_tracing_layer())
            .try_init()
            .wrap_err("new tracing subscriber try_init()")?;
    };

    if LevelFilter::current() != log_level {
        Err(eyre::eyre!(
            "not expected current log level {} when expected: {log_level}",
            LevelFilter::current()
        ))
    } else {
        tracing::info!(
            configured_log_level = %LevelFilter::current(),
            "current log level",
        );
        Ok(Logging {
            filter: ActiveLevel::new(
                initial_log_filter,
                Some(Box::new(ReloadableFilter::new(logfmt_stdout_reload_handle))),
            )
            .into(),
            tracing_enabled,
            spancount_reader: Some(spancount_reader),
            log_stream,
        })
    }
}

fn build_span_exporter(
    endpoint: &str,
) -> Result<opentelemetry_otlp::SpanExporter, opentelemetry_otlp::ExporterBuildError> {
    opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_protocol(opentelemetry_otlp::Protocol::Grpc)
        .with_endpoint(endpoint)
        .build()
}

#[derive(Debug, Clone)]
struct CarbideSpanSampler(Arc<AtomicBool>);

const CARBIDE_TRACE_ROOT_ATTRIBUTE: &str = "carbide.trace_root";

impl CarbideSpanSampler {
    fn new(enabled: Arc<AtomicBool>) -> Self {
        Self(enabled)
    }
}

/// Predicate to check if a child span or event should be accepted. This is distinct from
/// CarbideSpanSampler, which chooses which *root* spans to accept (ie. just ours). This predicate
/// checks if any span or event should be accepted, even within a root span.
///
/// Currently discards tokio spans: tokio seems to have an issue where it creates spans without
/// closing them, which results in us running out of memory quickly.
fn should_accept_span_or_event(metadata: &tracing::Metadata<'_>) -> bool {
    let is_tokio = metadata
        .module_path()
        .is_some_and(|p| p.starts_with("tokio"));

    !is_tokio
}

impl ShouldSample for CarbideSpanSampler {
    fn should_sample(
        &self,
        parent_context: Option<&Context>,
        _trace_id: TraceId,
        _name: &str,
        _span_kind: &SpanKind,
        attributes: &[KeyValue],
        _links: &[Link],
    ) -> SamplingResult {
        let enabled = self.0.load(Ordering::Relaxed);

        let parent_span_context = parent_context
            .map(|cx| cx.span().span_context().clone())
            .filter(|sc| sc.is_valid());

        let decision = if !enabled {
            SamplingDecision::Drop
        } else if let Some(local_parent) = parent_span_context.as_ref().filter(|sc| !sc.is_remote())
        {
            // In-process parent: inherit its sampling decision.
            if local_parent.is_sampled() {
                SamplingDecision::RecordAndSample
            } else {
                SamplingDecision::Drop
            }
        } else if is_carbide_root_span(attributes) {
            // Root or remote-parented span: record iff explicitly marked `carbide.trace_root`.
            SamplingDecision::RecordAndSample
        } else {
            SamplingDecision::Drop
        };

        SamplingResult {
            decision,
            attributes: vec![],
            // Carry any inbound `tracestate` so it propagates onward unchanged.
            trace_state: parent_span_context
                .map(|sc| sc.trace_state().clone())
                .unwrap_or_default(),
        }
    }
}

fn is_carbide_root_span(attributes: &[KeyValue]) -> bool {
    attributes.iter().any(|kv| {
        kv.key.as_str() == CARBIDE_TRACE_ROOT_ATTRIBUTE && matches!(&kv.value, Value::Bool(true))
    })
}
#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;
    use std::time::Duration;

    use opentelemetry::KeyValue;
    use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
        TraceService, TraceServiceServer,
    };
    use opentelemetry_proto::tonic::collector::trace::v1::{
        ExportTraceServiceRequest, ExportTraceServiceResponse,
    };
    use opentelemetry_sdk::testing::trace::new_test_export_span_data;
    use opentelemetry_sdk::trace::SpanExporter;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::TcpListenerStream;
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct TraceReceiver {
        requests: mpsc::Sender<ExportTraceServiceRequest>,
    }

    #[tonic::async_trait]
    impl TraceService for TraceReceiver {
        async fn export(
            &self,
            request: tonic::Request<ExportTraceServiceRequest>,
        ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
            self.requests
                .try_send(request.into_inner())
                .map_err(|error| tonic::Status::internal(error.to_string()))?;
            Ok(tonic::Response::new(ExportTraceServiceResponse::default()))
        }
    }

    #[tokio::test]
    async fn tracing_exporter_sends_spans_over_ipv6() {
        let listener = tokio::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind IPv6 OTLP receiver");
        let address = listener.local_addr().expect("OTLP receiver address");
        let exporter = build_span_exporter(&format!("http://{address}"))
            .expect("build production tracing exporter");
        let mut span = new_test_export_span_data();
        span.name = "ipv6-dependency-exchange".into();
        let trace_id = span.span_context.trace_id().to_bytes();
        let (requests, mut received) = mpsc::channel(1);
        let cancellation = CancellationToken::new();
        let server = tonic::transport::Server::builder()
            .add_service(TraceServiceServer::new(TraceReceiver { requests }));

        let (export_result, server_result) = tokio::join!(
            async {
                let result =
                    tokio::time::timeout(Duration::from_secs(10), exporter.export(vec![span]))
                        .await;
                drop(exporter);
                cancellation.cancel();
                result
            },
            tokio::time::timeout(
                Duration::from_secs(15),
                server.serve_with_incoming_shutdown(
                    TcpListenerStream::new(listener),
                    cancellation.cancelled(),
                ),
            ),
        );

        server_result
            .expect("IPv6 OTLP receiver did not stop within 15 seconds")
            .expect("serve IPv6 OTLP receiver");
        export_result
            .expect("IPv6 OTLP export did not finish within 10 seconds")
            .expect("export span over IPv6");
        let request = received.try_recv().expect("receive OTLP Export request");
        let [resource] = request.resource_spans.as_slice() else {
            panic!("expected one exported resource");
        };
        let [scope] = resource.scope_spans.as_slice() else {
            panic!("expected one exported scope");
        };
        let [span] = scope.spans.as_slice() else {
            panic!("expected one exported span");
        };
        assert_eq!(span.name, "ipv6-dependency-exchange");
        assert_eq!(span.trace_id, trace_id);
    }

    fn sample_decision(enabled: bool, attributes: Vec<KeyValue>) -> SamplingDecision {
        CarbideSpanSampler::new(Arc::new(AtomicBool::new(enabled)))
            .should_sample(
                None,
                TraceId::from(1),
                "test-span",
                &SpanKind::Internal,
                &attributes,
                &[],
            )
            .decision
    }

    #[test]
    fn sampler_accepts_marked_root_spans_when_enabled() {
        let decision = sample_decision(
            true,
            vec![KeyValue::new(CARBIDE_TRACE_ROOT_ATTRIBUTE, true)],
        );

        assert!(matches!(decision, SamplingDecision::RecordAndSample));
    }

    #[test]
    fn sampler_drops_marked_root_spans_when_disabled() {
        let decision = sample_decision(
            false,
            vec![KeyValue::new(CARBIDE_TRACE_ROOT_ATTRIBUTE, true)],
        );

        assert!(matches!(decision, SamplingDecision::Drop));
    }

    #[test]
    fn sampler_drops_unmarked_root_spans() {
        let decision = sample_decision(true, vec![KeyValue::new("span_id", "0xabc")]);

        assert!(matches!(decision, SamplingDecision::Drop));
    }

    #[test]
    fn sampler_drops_false_trace_root_markers() {
        let decision = sample_decision(
            true,
            vec![KeyValue::new(CARBIDE_TRACE_ROOT_ATTRIBUTE, false)],
        );

        assert!(matches!(decision, SamplingDecision::Drop));
    }

    #[test]
    fn sampler_drops_string_trace_root_markers() {
        let decision = sample_decision(
            true,
            vec![KeyValue::new(CARBIDE_TRACE_ROOT_ATTRIBUTE, "true")],
        );

        assert!(matches!(decision, SamplingDecision::Drop));
    }

    fn sample_decision_with_parent(
        enabled: bool,
        parent: Option<&Context>,
        attributes: Vec<KeyValue>,
    ) -> SamplingDecision {
        CarbideSpanSampler::new(Arc::new(AtomicBool::new(enabled)))
            .should_sample(
                parent,
                TraceId::from(1u128),
                "request",
                &SpanKind::Server,
                &attributes,
                &[],
            )
            .decision
    }

    /// A parent `Context` carrying a span context with the given sampled / remote flags.
    fn parent_ctx(sampled: bool, remote: bool) -> Context {
        use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceState};

        let sc = SpanContext::new(
            TraceId::from(0x1111_1111_1111_1111_1111_1111_1111_1111u128),
            SpanId::from(0x2222_2222_2222_2222u64),
            if sampled {
                TraceFlags::SAMPLED
            } else {
                TraceFlags::default()
            },
            remote,
            TraceState::default(),
        );
        Context::new().with_remote_span_context(sc)
    }

    fn root_marker() -> Vec<KeyValue> {
        vec![KeyValue::new(CARBIDE_TRACE_ROOT_ATTRIBUTE, true)]
    }

    #[test]
    fn sampler_disabled_drops_even_with_sampled_remote_parent() {
        // issue #2438: the local toggle is the master switch — an inbound sampled trace
        // must not be recorded while tracing is disabled locally.
        assert!(matches!(
            sample_decision_with_parent(false, Some(&parent_ctx(true, true)), root_marker()),
            SamplingDecision::Drop
        ));
    }

    #[test]
    fn sampler_remote_parent_gated_by_marker_not_inbound_sampled() {
        // A remote (ingress) parent never inherits its decision: the local `carbide.trace_root`
        // marker decides, whether or not the inbound trace was sampled.
        for remote_sampled in [true, false] {
            let parent = parent_ctx(remote_sampled, true);
            assert!(
                matches!(
                    sample_decision_with_parent(true, Some(&parent), root_marker()),
                    SamplingDecision::RecordAndSample
                ),
                "marked -> record (inbound sampled={remote_sampled})"
            );
            assert!(
                matches!(
                    sample_decision_with_parent(true, Some(&parent), vec![]),
                    SamplingDecision::Drop
                ),
                "unmarked -> drop (inbound sampled={remote_sampled})"
            );
        }
    }

    #[test]
    fn sampler_inherits_in_process_parent_decision() {
        // Sampled in-process parent pulls its subtree in, even without a marker.
        assert!(matches!(
            sample_decision_with_parent(true, Some(&parent_ctx(true, false)), vec![]),
            SamplingDecision::RecordAndSample
        ));
        // Unsampled in-process parent drops the subtree, even if the child is marked.
        assert!(matches!(
            sample_decision_with_parent(true, Some(&parent_ctx(false, false)), root_marker()),
            SamplingDecision::Drop
        ));
    }

    #[test]
    fn sampler_carries_parent_tracestate() {
        // The inbound `tracestate` must ride through the SamplingResult so it propagates onward
        // (issue #2438). A neutral, W3C-formatted single-entry tracestate keeps ordering trivial.
        use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceState};

        let trace_state = TraceState::from_key_value([("key1", "value1")]).unwrap();
        let parent = Context::new().with_remote_span_context(SpanContext::new(
            TraceId::from(0x1111_1111_1111_1111_1111_1111_1111_1111u128),
            SpanId::from(0x2222_2222_2222_2222u64),
            TraceFlags::SAMPLED,
            true,
            trace_state.clone(),
        ));

        let result = CarbideSpanSampler::new(Arc::new(AtomicBool::new(true))).should_sample(
            Some(&parent),
            TraceId::from(1u128),
            "request",
            &SpanKind::Server,
            &root_marker(),
            &[],
        );

        assert_eq!(result.trace_state, trace_state);
    }

    /// An OpenTelemetry layer is present (an exporter was built) but the `tracing-enabled` flag is
    /// off, so `CarbideSpanSampler` drops every span. Verifies what NICo forwards on egress in that
    /// state: does an inbound trace-id still reach a downstream service even though NICo records
    /// nothing?
    ///
    /// Inbound and egress headers are built and asserted as raw `traceparent` strings (the W3C wire
    /// format), so this test relies only on the propagation entry points NICo actually calls.
    #[test]
    fn egress_forwards_trace_id_when_exporter_on_but_flag_off() {
        use opentelemetry::trace::TracerProvider;
        use opentelemetry_sdk::propagation::TraceContextPropagator;
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use trace_propagation::{inject_current_context, set_span_parent_from_headers};
        use tracing_subscriber::prelude::*;

        global::set_text_map_propagator(TraceContextPropagator::new());

        // OTel layer present ("exporter on"), runtime flag OFF -> the sampler drops everything.
        let provider = SdkTracerProvider::builder()
            .with_sampler(CarbideSpanSampler::new(Arc::new(AtomicBool::new(false))))
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("carbide")));

        // Inbound: a sampled remote trace as a raw W3C traceparent (final `01` = sampled).
        let mut inbound = http::HeaderMap::new();
        inbound.insert(
            "traceparent",
            "00-11111111111111111111111111111111-2222222222222222-01"
                .parse()
                .unwrap(),
        );

        let mut egress = http::HeaderMap::new();
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("request");
            set_span_parent_from_headers(&span, &inbound);
            let _enter = span.enter();
            inject_current_context(&mut egress);
        });

        let out = egress
            .get("traceparent")
            .expect("egress should still carry a traceparent")
            .to_str()
            .unwrap();
        // The inbound trace-id IS forwarded downstream even though NICo recorded nothing -- the
        // dropped request span still inherits it (with a fresh child span-id)...
        assert!(
            out.starts_with("00-11111111111111111111111111111111-"),
            "egress must forward the inbound trace-id: {out}"
        );
        // ...but it goes out marked NOT sampled, because NICo dropped its span.
        assert!(
            out.ends_with("-00"),
            "egress must be marked not-sampled (NICo dropped its span): {out}"
        );
    }
    #[test]
    fn log_stream_layer_absent_when_admin_ui_disabled() {
        use tracing_subscriber::prelude::*;

        let stream = LogStream::new(8, 64 * 1024);
        let layer = admin_ui_log_stream_layer(false, &stream);
        assert!(layer.is_none(), "disabled admin UI must not build a layer");

        // Composed the way `setup_logging` composes it, the `None` layer is a
        // no-op: events reach neither live subscribers nor the replay ring.
        let mut rx = stream.subscribe();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("not streamed");
        });
        assert!(
            rx.try_recv().is_err(),
            "no line may be broadcast with the admin UI disabled"
        );
        assert!(
            stream.latest(10).is_empty(),
            "no history may be retained with the admin UI disabled"
        );
    }

    #[test]
    fn log_stream_layer_present_when_admin_ui_enabled() {
        use tracing_subscriber::prelude::*;

        let stream = LogStream::new(8, 64 * 1024);
        let mut rx = stream.subscribe();
        let layer = admin_ui_log_stream_layer(true, &stream);
        assert!(layer.is_some(), "enabled admin UI must build the layer");

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("streamed");
        });
        let line = rx.try_recv().expect("a line should have been broadcast");
        assert_eq!(line.message, "streamed");
        assert_eq!(stream.latest(10).len(), 1);
    }

    /// Install `dep_log_filter(user_directives)` as the thread-local subscriber
    /// for the duration of `f`, so `tracing::enabled!` calls inside reflect the
    /// effective filter.
    fn with_filter<R>(user_directives: &str, f: impl FnOnce() -> R) -> R {
        use tracing_subscriber::prelude::*;

        let user = EnvFilter::builder().parse(user_directives).unwrap();
        let subscriber = tracing_subscriber::registry().with(dep_log_filter(user));
        tracing::subscriber::with_default(subscriber, f)
    }

    #[test]
    fn user_directives_override_defaults() {
        with_filter("info,vaultrs=debug,rustify=trace", || {
            assert!(
                tracing::enabled!(target: "vaultrs", tracing::Level::DEBUG),
                "user's vaultrs=debug should win over the dep cap"
            );
            assert!(
                tracing::enabled!(target: "rustify", tracing::Level::TRACE),
                "user's rustify=trace should win over rustify=off"
            );
            // Unspecified dep target still capped at error.
            assert!(
                !tracing::enabled!(target: "hyper", tracing::Level::INFO),
                "hyper should still be capped at error by dep default"
            );
        });
    }

    #[test]
    fn bare_default_does_not_override_dep_defaults() {
        // RUST_LOG=info; user only sets a default, no per-target directives.
        with_filter("info", || {
            // User's default applies to unrelated targets.
            assert!(tracing::enabled!(target: "carbide", tracing::Level::INFO));
            assert!(!tracing::enabled!(target: "carbide", tracing::Level::DEBUG));
            // Dep caps still apply where the user didn't override.
            assert!(!tracing::enabled!(target: "hyper", tracing::Level::INFO));
            assert!(tracing::enabled!(target: "hyper", tracing::Level::ERROR));
            assert!(!tracing::enabled!(target: "vaultrs", tracing::Level::INFO));
            assert!(tracing::enabled!(target: "vaultrs", tracing::Level::ERROR));
        });
    }

    #[test]
    fn user_target_overrides_default_without_touching_others() {
        // RUST_LOG=info,carbide=debug; raises one target; deps stay capped.
        with_filter("info,carbide=debug", || {
            assert!(tracing::enabled!(target: "carbide", tracing::Level::DEBUG));
            // Unrelated target still at the INFO default.
            assert!(tracing::enabled!(target: "other", tracing::Level::INFO));
            assert!(!tracing::enabled!(target: "other", tracing::Level::DEBUG));
            // Dep caps unaffected.
            assert!(!tracing::enabled!(target: "hyper", tracing::Level::INFO));
        });
    }

    #[test]
    fn unmentioned_dep_default_stays_when_user_raises_another() {
        // User raises vaultrs but says nothing about hyper, hyper stays default.
        with_filter("info,vaultrs=trace", || {
            assert!(tracing::enabled!(target: "vaultrs", tracing::Level::TRACE));
            assert!(!tracing::enabled!(target: "hyper", tracing::Level::INFO));
            assert!(tracing::enabled!(target: "hyper", tracing::Level::ERROR));
        });
    }

    #[test]
    fn regression_debug_default_directive_survives_dep_filter() {
        // Make sure with_default_directive is not ignored
        let initial = EnvFilter::builder()
            .with_default_directive(LevelFilter::DEBUG.into())
            .parse("")
            .unwrap();

        let subscriber = tracing_subscriber::registry().with(dep_log_filter(initial));
        tracing::subscriber::with_default(subscriber, || {
            assert!(tracing::enabled!(target: "carbide", tracing::Level::DEBUG));
            assert!(!tracing::enabled!(target: "carbide", tracing::Level::TRACE));
        });
    }
}
