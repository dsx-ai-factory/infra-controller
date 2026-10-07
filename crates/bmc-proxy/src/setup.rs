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

use metrics_endpoint::MetricsSetup;
use opentelemetry::trace::TracerProvider;
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, filter};

use crate::config::TracingConfig;

#[derive(thiserror::Error, Debug)]
pub(crate) enum SetupError {
    #[error("error configuring logging from environment variables: {0}")]
    EnvFilter(#[from] tracing_subscriber::filter::FromEnvError),
    #[error("error initializing tracing subscriber: {0}")]
    TracingSubscriberInit(#[from] tracing_subscriber::util::TryInitError),
    #[error("error setting up metrics: {0}")]
    Metrics(String),
}

pub(crate) type SetupResult<T> = Result<T, SetupError>;

/// Owns the OTLP tracer provider for the lifetime of the process. The batch
/// span processor only exports on its own schedule, so the provider has to be
/// shut down explicitly at exit or the final batch is dropped.
pub(crate) struct TracingGuard(Option<opentelemetry_sdk::trace::SdkTracerProvider>);

impl TracingGuard {
    /// Flushes and shuts down the OTLP exporter. `SdkTracerProvider::shutdown`
    /// blocks the calling thread for up to five seconds waiting on the batch
    /// processor's worker, so it must not run on a runtime worker.
    pub(crate) async fn shutdown(self) {
        let Some(provider) = self.0 else {
            return;
        };

        match tokio::task::spawn_blocking(move || provider.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(%error, "failed to flush OpenTelemetry spans on shutdown");
            }
            Err(error) => {
                tracing::warn!(%error, "OpenTelemetry shutdown task failed");
            }
        }
    }
}

pub(crate) fn setup_logging(
    debug: bool,
    tracing_config: &TracingConfig,
) -> SetupResult<TracingGuard> {
    // W3C propagation must be installed before any inbound extract or outbound inject (#2438).
    // Without it, OpenTelemetry's default propagator is a no-op.
    global::set_text_map_propagator(TraceContextPropagator::new());

    let default_log_level = if debug {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };

    let log_filter = dep_log_filter(
        EnvFilter::builder()
            .with_default_directive(default_log_level.into())
            .from_env()?,
    );

    let tracing_enabled = debug || tracing_config.enabled;
    let trace_filter = filter::filter_fn(move |metadata: &tracing::Metadata<'_>| {
        tracing_enabled && should_accept_span_or_event(metadata)
    })
    .with_max_level_hint(default_log_level);

    let endpoint = otlp_endpoint(
        |var| std::env::var(var).ok(),
        tracing_config.otlp_endpoint.as_deref(),
    );

    // Span export is a diagnostic aid, not part of the proxy's contract, so a
    // rejected endpoint degrades to no tracing rather than failing startup and
    // cutting off BMC access. The subscriber does not exist yet, so the failure
    // is carried past initialization and reported below.
    let (tracer_provider, exporter_error) = match endpoint.as_deref().map(build_span_exporter) {
        None => (None, None),
        Some(Ok(exporter)) => (Some(build_tracer_provider(exporter)), None),
        Some(Err(error)) => (None, Some(error)),
    };

    let maybe_otel_tracing_layer = tracer_provider.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("nico-bmc-proxy"))
            .with_filter(trace_filter)
    });

    let log_events = carbide_instrument::LogEventsMetric::new("nico-bmc-proxy");
    tracing_subscriber::registry()
        .with(log_events.layer().with_filter(log_filter.clone()))
        .with(maybe_otel_tracing_layer)
        .with(
            logfmt::layer()
                .with_event_fields([logfmt::EventField::with_default(
                    "component",
                    "nico-bmc-proxy",
                )])
                .with_filter(log_filter),
        )
        .try_init()?;

    tracing::info!(
        configured_log_level = %LevelFilter::current(),
        tracing_enabled,
        "current log level"
    );

    if let Some(error) = exporter_error {
        tracing::warn!(
            %error,
            endpoint = endpoint.as_deref().unwrap_or_default(),
            "OpenTelemetry span export disabled; proxy continues without tracing"
        );
    }

    Ok(TracingGuard(tracer_provider))
}

/// The standard OTLP endpoint variables, in the order `TonicExporterBuilder::resolve_endpoint`
/// itself consults them: signal-specific first, then the one covering every signal.
const OTLP_ENDPOINT_VARS: [&str; 2] = [
    opentelemetry_otlp::OTEL_EXPORTER_OTLP_TRACES_ENDPOINT,
    opentelemetry_otlp::OTEL_EXPORTER_OTLP_ENDPOINT,
];

/// Resolves the collector endpoint, preferring the standard OTLP variables over the config TOML.
/// `env` is a parameter so the precedence is testable without mutating the process environment.
///
/// The variables are read here, mirroring the exporter builder's own order, rather than left to the
/// builder: absent any configuration it defaults to `http://localhost:4317` and would ship spans at
/// a collector nobody asked for. Returning `None` is what holds span export off entirely, and that
/// decision needs the same view of the environment the builder has.
fn otlp_endpoint(
    env: impl Fn(&str) -> Option<String>,
    config_endpoint: Option<&str>,
) -> Option<String> {
    // An empty endpoint counts as unset on either path, matching how the builder treats an empty
    // programmatic value.
    OTLP_ENDPOINT_VARS
        .iter()
        .find_map(|var| env(var).filter(|endpoint| !endpoint.is_empty()))
        .or_else(|| {
            config_endpoint
                .filter(|endpoint| !endpoint.is_empty())
                .map(str::to_string)
        })
}

fn build_span_exporter(
    endpoint: &str,
) -> Result<opentelemetry_otlp::SpanExporter, opentelemetry_otlp::ExporterBuildError> {
    // `with_tonic` already selects OTLP/gRPC. The rest of the transport — timeout, compression,
    // TLS, headers — is left to the standard `OTEL_EXPORTER_OTLP_*` variables the builder reads.
    opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
}

fn build_tracer_provider(
    exporter: opentelemetry_otlp::SpanExporter,
) -> opentelemetry_sdk::trace::SdkTracerProvider {
    opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_attributes([KeyValue::new("service.name", "nico-bmc-proxy")])
                .build(),
        )
        .build()
}

/// Tokio runtime spans are not closed reliably; exporting them would leak memory.
fn should_accept_span_or_event(metadata: &tracing::Metadata<'_>) -> bool {
    !metadata
        .module_path()
        .is_some_and(|path| path.starts_with("tokio"))
}

pub(crate) fn setup_metrics() -> SetupResult<MetricsSetup> {
    metrics_endpoint::new_metrics_setup("carbide-bmc-proxy", "carbide-system", true)
        .map_err(|e| SetupError::Metrics(e.to_string()))
}

fn dep_log_filter(env_filter: EnvFilter) -> EnvFilter {
    [
        "hyper=error",
        "rustls=warn",
        "tokio_util::codec=warn",
        "vaultrs=error",
        "h2=warn",
    ]
    .iter()
    .fold(env_filter, |f, filter_str| {
        f.add_directive(
            filter_str
                .parse()
                .unwrap_or_else(|err| panic!("{filter_str} must be parsed; error: {err}")),
        )
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy)]
    enum FilterCase {
        DefaultAllowsApplicationInfo,
        DefaultSuppressesHyperInfo,
        DefaultAllowsHyperError,
        UserOverrideAllowsApplicationDebug,
        DependencyCapOverridesVaultrsDebug,
        UserOverrideDoesNotAffectHyperCap,
    }

    fn filter_allows(case: FilterCase) -> bool {
        let directives = match case {
            FilterCase::DefaultAllowsApplicationInfo
            | FilterCase::DefaultSuppressesHyperInfo
            | FilterCase::DefaultAllowsHyperError => "info",
            FilterCase::UserOverrideAllowsApplicationDebug => "info,carbide_bmc_proxy=debug",
            FilterCase::DependencyCapOverridesVaultrsDebug
            | FilterCase::UserOverrideDoesNotAffectHyperCap => "info,vaultrs=debug",
        };
        let user = EnvFilter::builder().parse(directives).unwrap();
        let subscriber = tracing_subscriber::registry().with(dep_log_filter(user));

        tracing::subscriber::with_default(subscriber, || match case {
            FilterCase::DefaultAllowsApplicationInfo => {
                tracing::enabled!(target: "carbide_bmc_proxy", tracing::Level::INFO)
            }
            FilterCase::DefaultSuppressesHyperInfo => {
                tracing::enabled!(target: "hyper", tracing::Level::INFO)
            }
            FilterCase::DefaultAllowsHyperError => {
                tracing::enabled!(target: "hyper", tracing::Level::ERROR)
            }
            FilterCase::UserOverrideAllowsApplicationDebug => {
                tracing::enabled!(target: "carbide_bmc_proxy", tracing::Level::DEBUG)
            }
            FilterCase::DependencyCapOverridesVaultrsDebug => {
                tracing::enabled!(target: "vaultrs", tracing::Level::DEBUG)
            }
            FilterCase::UserOverrideDoesNotAffectHyperCap => {
                tracing::enabled!(target: "hyper", tracing::Level::INFO)
            }
        })
    }

    #[test]
    fn dependency_log_filter_applies_caps_and_user_overrides() {
        value_scenarios!(
            run = filter_allows;
            "application info allowed by default directive" {
                FilterCase::DefaultAllowsApplicationInfo => true,
            }

            "hyper info suppressed by dependency cap" {
                FilterCase::DefaultSuppressesHyperInfo => false,
            }

            "hyper error allowed by dependency cap" {
                FilterCase::DefaultAllowsHyperError => true,
            }

            "user override allows application debug" {
                FilterCase::UserOverrideAllowsApplicationDebug => true,
            }

            "dependency cap overrides vaultrs debug" {
                FilterCase::DependencyCapOverridesVaultrsDebug => false,
            }

            "user override leaves unrelated dependency capped" {
                FilterCase::UserOverrideDoesNotAffectHyperCap => false,
            }
        );
    }

    // The tonic channel is built lazily on the ambient runtime, so the
    // accepted-endpoint case needs a runtime even though nothing connects.
    #[tokio::test]
    async fn span_exporter_build_validates_endpoint_eagerly() {
        value_scenarios!(
            run = |endpoint| build_span_exporter(endpoint).is_ok();
            "well-formed collector endpoint is accepted" {
                "http://otel-collector.observability.svc.cluster.local:4317" => true,
            }

            "malformed endpoint is rejected at build time rather than at first export" {
                "http://otel collector:4317" => false,
            }
        );
    }

    /// The inputs [`otlp_endpoint`] weighs against each other, named so a scenario reads as the
    /// deployment it stands for.
    #[derive(Clone, Copy)]
    struct EndpointInputs {
        signal_var: Option<&'static str>,
        generic_var: Option<&'static str>,
        config: Option<&'static str>,
    }

    fn resolved_endpoint(inputs: EndpointInputs) -> Option<String> {
        otlp_endpoint(
            |var| {
                if var == opentelemetry_otlp::OTEL_EXPORTER_OTLP_TRACES_ENDPOINT {
                    inputs.signal_var.map(str::to_string)
                } else if var == opentelemetry_otlp::OTEL_EXPORTER_OTLP_ENDPOINT {
                    inputs.generic_var.map(str::to_string)
                } else {
                    panic!("unexpected endpoint variable: {var}")
                }
            },
            inputs.config,
        )
    }

    #[test]
    fn otlp_endpoint_prefers_standard_variables_over_config() {
        // Distinct per source so a failure names the one that wrongly won. Syntactically valid
        // endpoints, since the exporter builder rejects malformed ones.
        const SIGNAL_ENDPOINT: &str = "http://signal-collector:4317";
        const GENERIC_ENDPOINT: &str = "http://generic-collector:4317";
        const CONFIG_ENDPOINT: &str = "http://config-collector:4317";
        const NOTHING_SET: EndpointInputs = EndpointInputs {
            signal_var: None,
            generic_var: None,
            config: None,
        };

        value_scenarios!(
            run = resolved_endpoint;
            "nothing configured leaves span export off" {
                NOTHING_SET => None,
            }

            "signal-specific variable is used" {
                EndpointInputs { signal_var: Some(SIGNAL_ENDPOINT), ..NOTHING_SET }
                    => Some(SIGNAL_ENDPOINT.to_string()),
            }

            "generic variable is honored too" {
                EndpointInputs { generic_var: Some(GENERIC_ENDPOINT), ..NOTHING_SET }
                    => Some(GENERIC_ENDPOINT.to_string()),
            }

            "signal-specific variable wins over the generic one" {
                EndpointInputs {
                    signal_var: Some(SIGNAL_ENDPOINT),
                    generic_var: Some(GENERIC_ENDPOINT),
                    ..NOTHING_SET
                } => Some(SIGNAL_ENDPOINT.to_string()),
            }

            "signal-specific variable overrides the config file" {
                EndpointInputs {
                    signal_var: Some(SIGNAL_ENDPOINT),
                    config: Some(CONFIG_ENDPOINT),
                    ..NOTHING_SET
                } => Some(SIGNAL_ENDPOINT.to_string()),
            }

            "generic variable overrides the config file" {
                EndpointInputs {
                    generic_var: Some(GENERIC_ENDPOINT),
                    config: Some(CONFIG_ENDPOINT),
                    ..NOTHING_SET
                } => Some(GENERIC_ENDPOINT.to_string()),
            }

            "config file is used when no variable is set" {
                EndpointInputs { config: Some(CONFIG_ENDPOINT), ..NOTHING_SET }
                    => Some(CONFIG_ENDPOINT.to_string()),
            }

            "empty config endpoint counts as unset" {
                EndpointInputs { config: Some(""), ..NOTHING_SET }
                    => None,
            }

            "empty variable does not shadow the generic one" {
                EndpointInputs {
                    signal_var: Some(""),
                    generic_var: Some(GENERIC_ENDPOINT),
                    ..NOTHING_SET
                } => Some(GENERIC_ENDPOINT.to_string()),
            }

            "empty variable falls through to the config file" {
                EndpointInputs {
                    signal_var: Some(""),
                    config: Some(CONFIG_ENDPOINT),
                    ..NOTHING_SET
                } => Some(CONFIG_ENDPOINT.to_string()),
            }
        );
    }

    #[test]
    fn should_accept_span_or_event_accepts_application_spans() {
        assert!(should_accept_span_or_event(
            tracing::info_span!(target: "carbide_bmc_proxy", "bmc_proxy_request")
                .metadata()
                .unwrap()
        ));
    }

    #[test]
    fn metrics_setup_initializes_health_controller() {
        // Mirrors setup_metrics() without its global-meter install: the
        // process-wide test meter (installed at load by
        // carbide_instrument::testing) owns instrument bindings for this
        // binary's event tests, and swapping the global provider mid-run
        // would steal first-emit bindings from them.
        let setup =
            metrics_endpoint::new_metrics_setup("carbide-bmc-proxy", "carbide-system", false)
                .expect("metrics setup succeeds");

        assert!(setup.health_controller.is_ready());
        assert!(setup.health_controller.is_healthy());
    }
}
