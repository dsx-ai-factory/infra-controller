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

//! Builds the OTLP span-export layer for service binaries.
//!
//! Exports spans only when a collector endpoint is set. A bad endpoint logs a
//! warning and the service keeps running.
//!
//! The layer carries its own span filter, so changing the span level does not
//! change log output. Attach the log `EnvFilter` to each log layer rather than
//! the registry, since a registry filter also applies to this layer.
//!
//! ```no_run
//! use tracing_subscriber::Layer;
//! use tracing_subscriber::layer::SubscriberExt;
//! use tracing_subscriber::util::SubscriberInitExt;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let log_filter = tracing_subscriber::EnvFilter::builder()
//!     .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
//!     .from_env_lossy();
//!
//! let (span_layer, tracing) = carbide_instrument::otlp_tracing::setup(
//!     carbide_instrument::otlp_tracing::Config::new("nico-pxe"),
//! );
//!
//! tracing_subscriber::registry()
//!     .with(span_layer)
//!     .with(logfmt::layer().with_filter(log_filter))
//!     .try_init()?;
//!
//! tracing.report();
//! // ... run the service ...
//! tracing.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! # Shutdown
//!
//! The exporter sends spans in batches on a timer. Call [`Tracing::shutdown`]
//! before the process exits to send the last batch.
//!
//! # Sampling
//!
//! This module installs no sampler, so `OTEL_TRACES_SAMPLER` and
//! `OTEL_TRACES_SAMPLER_ARG` take effect. Prefer `parentbased_traceidratio`,
//! which applies the ratio only where a trace starts. A sampler drops spans
//! before the collector sees them, so keep the default if the collector picks
//! traces by latency or errors.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::registry::LookupSpan;

/// Standard OTLP endpoint variable for traces only.
const TRACES_ENDPOINT_VAR: &str = opentelemetry_otlp::OTEL_EXPORTER_OTLP_TRACES_ENDPOINT;

/// Standard OTLP endpoint variable that also applies to metrics and logs.
const GENERIC_ENDPOINT_VAR: &str = opentelemetry_otlp::OTEL_EXPORTER_OTLP_ENDPOINT;

/// Both endpoint variables, in precedence order.
const ENDPOINT_VARS: [&str; 2] = [TRACES_ENDPOINT_VAR, GENERIC_ENDPOINT_VAR];

/// Controls span verbosity independently of `RUST_LOG`.
pub const SPAN_LEVEL_VAR: &str = "NICO_TRACES_SPAN_LEVEL";

/// Collector and filtering settings for span export.
#[derive(Debug, Clone)]
pub struct Config {
    /// Sets `service.name` on the exported spans and names the tracer.
    pub service_name: &'static str,
    /// Collector endpoint from the service's config file or CLI flags. The
    /// standard OTLP variables override it.
    pub config_endpoint: Option<String>,
    /// Most verbose span level exported when [`SPAN_LEVEL_VAR`] is unset.
    pub default_span_level: LevelFilter,
    /// Excludes the crates listed in [`TRANSPORT_CRATES`] from span export.
    pub exclude_transport_crates: bool,
}

impl Config {
    /// Exports spans at `INFO`, and only when an OTLP variable sets an endpoint.
    pub fn new(service_name: &'static str) -> Self {
        Self {
            service_name,
            config_endpoint: None,
            default_span_level: LevelFilter::INFO,
            exclude_transport_crates: true,
        }
    }

    /// Sets the fallback endpoint used when no OTLP variable is set.
    #[must_use]
    pub fn with_config_endpoint(mut self, endpoint: Option<String>) -> Self {
        self.config_endpoint = endpoint;
        self
    }

    /// Sets span verbosity when [`SPAN_LEVEL_VAR`] is unset.
    #[must_use]
    pub fn with_default_span_level(mut self, level: LevelFilter) -> Self {
        self.default_span_level = level;
        self
    }

    /// Includes [`TRANSPORT_CRATES`], which can make export generate more spans.
    /// Prefer `RUST_LOG` when debugging the transport.
    #[must_use]
    pub fn export_transport_crates(mut self) -> Self {
        self.exclude_transport_crates = false;
        self
    }
}

/// The span-export layer to pass to `SubscriberExt::with`.
///
/// `None` disables export when the endpoint is unset or rejected.
pub type SpanLayer<S> = Option<Box<dyn Layer<S> + Send + Sync>>;

/// Keeps the tracer provider alive while the process runs.
pub struct Tracing {
    provider: Option<SdkTracerProvider>,
    state: State,
    span_level: LevelFilter,
    invalid_span_level: Option<String>,
}

enum State {
    Off,
    On {
        endpoint: String,
    },
    Failed {
        endpoint: String,
        error: opentelemetry_otlp::ExporterBuildError,
    },
}

impl Tracing {
    /// Logs export settings and errors. Call after initializing the subscriber.
    pub fn report(&self) {
        match &self.state {
            State::Off => {
                tracing::debug!(
                    traces_var = TRACES_ENDPOINT_VAR,
                    generic_var = GENERIC_ENDPOINT_VAR,
                    "no OTLP endpoint configured; span export off"
                );
            }
            State::On { endpoint } => {
                tracing::info!(
                    endpoint = %endpoint,
                    span_level = %self.span_level,
                    "exporting spans over OTLP/gRPC"
                );
            }
            State::Failed { endpoint, error } => {
                tracing::warn!(
                    endpoint = %endpoint,
                    %error,
                    "OTLP span exporter could not be built; continuing without span export"
                );
            }
        }

        if let Some(value) = &self.invalid_span_level {
            tracing::warn!(
                var = SPAN_LEVEL_VAR,
                %value,
                fallback = %self.span_level,
                "ignoring unparseable span level"
            );
        }
    }

    /// Flushes pending spans and stops the exporter; a no-op when export is off.
    /// Runs on a blocking thread because shutdown can wait up to five seconds.
    pub async fn shutdown(self) {
        let Some(provider) = self.provider else {
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

/// Builds the span-export layer and installs the W3C trace-context propagator.
///
/// Requires a Tokio runtime. Call before initializing the subscriber, then call
/// [`Tracing::report`] to log the result. An unset or rejected endpoint disables
/// export. The propagator lets services share a trace through `traceparent`.
pub fn setup<S>(config: Config) -> (SpanLayer<S>, Tracing)
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    global::set_text_map_propagator(TraceContextPropagator::new());

    let (span_level, invalid_span_level) =
        span_level(|var| std::env::var(var).ok(), config.default_span_level);
    let endpoint = endpoint(
        |var| std::env::var(var).ok(),
        config.config_endpoint.as_deref(),
    );

    let (provider, state) = match endpoint {
        None => (None, State::Off),
        Some(endpoint) => match build_span_exporter(&endpoint) {
            Ok(exporter) => (
                Some(build_tracer_provider(exporter, config.service_name)),
                State::On { endpoint },
            ),
            Err(error) => (None, State::Failed { endpoint, error }),
        },
    };

    let exclude_transport_crates = config.exclude_transport_crates;
    let layer = provider.as_ref().map(|provider| {
        let filter = tracing_subscriber::filter::filter_fn(move |metadata| {
            exportable(
                span_level,
                exclude_transport_crates,
                metadata.level(),
                metadata.module_path(),
            )
        })
        // Lets tracing skip levels that no layer accepts.
        .with_max_level_hint(span_level);
        let layer: Box<dyn Layer<S> + Send + Sync> = Box::new(
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer(config.service_name))
                .with_filter(filter),
        );
        layer
    });

    (
        layer,
        Tracing {
            provider,
            state,
            span_level,
            invalid_span_level,
        },
    )
}

/// Prefers OTLP variables over config. Without either, disables export instead
/// of using the SDK's localhost default.
fn endpoint(env: impl Fn(&str) -> Option<String>, config_endpoint: Option<&str>) -> Option<String> {
    ENDPOINT_VARS
        .iter()
        .find_map(|var| env(var).filter(|endpoint| !endpoint.is_empty()))
        .or_else(|| {
            config_endpoint
                .filter(|endpoint| !endpoint.is_empty())
                .map(str::to_string)
        })
}

/// Uses the default for an invalid level and returns the bad value for logging.
fn span_level(
    env: impl Fn(&str) -> Option<String>,
    default: LevelFilter,
) -> (LevelFilter, Option<String>) {
    match env(SPAN_LEVEL_VAR).filter(|value| !value.is_empty()) {
        None => (default, None),
        Some(value) => match value.trim().parse::<LevelFilter>() {
            Ok(level) => (level, None),
            Err(_) => (default, Some(value)),
        },
    }
}

fn build_span_exporter(
    endpoint: &str,
) -> Result<opentelemetry_otlp::SpanExporter, opentelemetry_otlp::ExporterBuildError> {
    // Use gRPC; let the SDK read timeout, compression, and header settings.
    opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
}

/// Leaves sampling to the SDK so `OTEL_TRACES_SAMPLER` works.
fn build_tracer_provider(
    exporter: opentelemetry_otlp::SpanExporter,
    service_name: &'static str,
) -> SdkTracerProvider {
    SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_attributes([KeyValue::new("service.name", service_name)])
                .build(),
        )
        .build()
}

/// Exclude long-lived Tokio spans to avoid retaining their events in memory.
const RUNTIME_CRATE: &str = "tokio";

/// The exporter's gRPC stack. Excluded by default because exporting its spans
/// can generate more spans to export.
pub const TRANSPORT_CRATES: [&str; 4] = ["h2", "hyper", "tonic", "tower"];

/// Filters exported spans and events independently of logs.
fn exportable(
    span_level: LevelFilter,
    exclude_transport_crates: bool,
    level: &tracing::Level,
    module_path: Option<&str>,
) -> bool {
    if *level > span_level {
        return false;
    }

    let Some(path) = module_path else {
        return true;
    };

    if in_crate(path, RUNTIME_CRATE) {
        return false;
    }

    !exclude_transport_crates
        || !TRANSPORT_CRATES
            .iter()
            .any(|crate_name| in_crate(path, crate_name))
}

/// Matches a module path against a crate name, so `tower_of_hanoi` does not match
/// `tower`.
fn in_crate(module_path: &str, crate_name: &str) -> bool {
    module_path
        .strip_prefix(crate_name)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy)]
    struct EndpointInputs {
        traces_var: Option<&'static str>,
        generic_var: Option<&'static str>,
        config: Option<&'static str>,
    }

    fn resolved_endpoint(inputs: EndpointInputs) -> Option<String> {
        endpoint(
            |var| {
                if var == opentelemetry_otlp::OTEL_EXPORTER_OTLP_TRACES_ENDPOINT {
                    inputs.traces_var.map(str::to_string)
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
    fn endpoint_prefers_standard_variables_over_config() {
        // One value per source, so a failure names the source that won.
        const TRACES: &str = "http://traces-collector:4317";
        const GENERIC: &str = "http://generic-collector:4317";
        const CONFIG: &str = "http://config-collector:4317";
        const NOTHING_SET: EndpointInputs = EndpointInputs {
            traces_var: None,
            generic_var: None,
            config: None,
        };

        value_scenarios!(
            run = resolved_endpoint;
            "nothing configured leaves span export off" {
                NOTHING_SET => None,
            }

            "the trace-only variable takes precedence over the shared one and the config" {
                EndpointInputs {
                    traces_var: Some(TRACES),
                    generic_var: Some(GENERIC),
                    config: Some(CONFIG),
                    } => Some(TRACES.to_string()),
            }

            "generic variable overrides the config file" {
                EndpointInputs { generic_var: Some(GENERIC), config: Some(CONFIG), ..NOTHING_SET }
                    => Some(GENERIC.to_string()),
            }

            "config file is used when no variable is set" {
                EndpointInputs { config: Some(CONFIG), ..NOTHING_SET }
                    => Some(CONFIG.to_string()),
            }

            "empty config endpoint counts as unset rather than as localhost" {
                EndpointInputs { config: Some(""), ..NOTHING_SET } => None,
            }

            "empty variable falls through instead of shadowing the config" {
                EndpointInputs { traces_var: Some(""), config: Some(CONFIG), ..NOTHING_SET }
                    => Some(CONFIG.to_string()),
            }
        );
    }

    #[test]
    fn span_level_falls_back_to_the_default_on_a_value_it_cannot_parse() {
        fn resolved(value: Option<&'static str>) -> (LevelFilter, Option<String>) {
            span_level(|_| value.map(str::to_string), LevelFilter::INFO)
        }

        value_scenarios!(
            run = resolved;
            "unset keeps the service default" {
                None => (LevelFilter::INFO, None),
            }

            "a level raises span export without touching the log filter" {
                Some("debug") => (LevelFilter::DEBUG, None),
            }

            "surrounding whitespace is tolerated" {
                Some(" trace ") => (LevelFilter::TRACE, None),
            }

            "off disables span export while the exporter stays configured" {
                Some("off") => (LevelFilter::OFF, None),
            }

            "empty counts as unset" {
                Some("") => (LevelFilter::INFO, None),
            }

            "a value it cannot parse is reported and the default level is kept" {
                Some("verbose") => (LevelFilter::INFO, Some("verbose".to_string())),
            }
        );
    }

    // The builder creates the gRPC channel on the current runtime, so this test
    // needs one even though it never connects to a collector.
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

    #[test]
    fn span_filter_gates_on_its_own_level_and_always_drops_excluded_crates() {
        struct FilterInputs {
            span_level: LevelFilter,
            exclude_transport_crates: bool,
            level: tracing::Level,
            module_path: &'static str,
        }

        fn allows(inputs: FilterInputs) -> bool {
            exportable(
                inputs.span_level,
                inputs.exclude_transport_crates,
                &inputs.level,
                Some(inputs.module_path),
            )
        }

        const APP: &str = "carbide_pxe::routes::ipxe";

        value_scenarios!(
            run = allows;
            "a span at the configured level is exported" {
                FilterInputs {
                    exclude_transport_crates: true,
                    span_level: LevelFilter::INFO,
                    level: tracing::Level::INFO,
                    module_path: APP,
                } => true,
            }

            "a span below the configured level is dropped, which keeps DEBUG spans out
             of a default deployment" {
                FilterInputs {
                    exclude_transport_crates: true,
                    span_level: LevelFilter::INFO,
                    level: tracing::Level::DEBUG,
                    module_path: APP,
                } => false,
            }

            "raising the span level exports it, without the log filter being consulted" {
                FilterInputs {
                    exclude_transport_crates: true,
                    span_level: LevelFilter::TRACE,
                    level: tracing::Level::DEBUG,
                    module_path: APP,
                } => true,
            }

            "tokio spans are dropped even when the transport crates are exported" {
                FilterInputs {
                    exclude_transport_crates: false,
                    span_level: LevelFilter::TRACE,
                    level: tracing::Level::INFO,
                    module_path: "tokio::runtime::task",
                } => false,
            }

            "spans from the exporter's own gRPC stack are dropped, so exporting does not
             create more spans to export" {
                FilterInputs {
                    exclude_transport_crates: true,
                    span_level: LevelFilter::TRACE,
                    level: tracing::Level::DEBUG,
                    module_path: "h2::proto::streams::send",
                } => false,
            }

            "opting in exports the gRPC stack, for debugging the transport itself" {
                FilterInputs {
                    exclude_transport_crates: false,
                    span_level: LevelFilter::TRACE,
                    level: tracing::Level::DEBUG,
                    module_path: "h2::proto::streams::send",
                } => true,
            }

            "a crate whose name only starts with an excluded name is still exported" {
                FilterInputs {
                    exclude_transport_crates: true,
                    span_level: LevelFilter::TRACE,
                    level: tracing::Level::INFO,
                    module_path: "tower_of_hanoi::solver",
                } => true,
            }

            "OFF stops export while the exporter stays configured" {
                FilterInputs {
                    exclude_transport_crates: true,
                    span_level: LevelFilter::OFF,
                    level: tracing::Level::ERROR,
                    module_path: APP,
                } => false,
            }
        );
    }
}
