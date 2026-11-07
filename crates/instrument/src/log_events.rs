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

//! `carbide_log_events_total{level, component}`: counts every log line a
//! binary emits, so its error/warn rate is visible and alertable from
//! Prometheus with no per-call-site work.
//!
//! Install in two decoupled steps, so the subscriber can come up before the
//! meter exists (the order every NICo binary already uses):
//!
//! ```ignore
//! let log_events = LogEventsMetric::new("nico-api");
//! tracing_subscriber::registry()
//!     .with(log_events.layer())        // 1. count events from startup
//!     .try_init()?;
//! // ... later, once the meter provider exists:
//! log_events::register(&meter);        // 2. expose the counts
//! ```
//!
//! The `component` label resolves like the logfmt `component` key: the nearest
//! enclosing span that set a `component` attribute wins, else the binary's
//! default. Component names must come from a small fixed set (subsystem names,
//! never per-request values); because they arrive as runtime strings rather
//! than through the framework's usual type-level label guard, the counts also
//! enforce the bound themselves -- beyond [`MAX_COMPONENTS`] distinct names,
//! new components are counted under `"other"`. Cardinality is therefore at
//! most `levels (5) x MAX_COMPONENTS`.
//!
//! Counting happens into shared atomics and the metric is an observable
//! counter reading them, so registration order can never bind anything to a
//! no-op meter. (A `LogLimiter`-gated call site suppresses before any event
//! fires, so its skipped logs are not -- and cannot be -- counted here.)

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;
use tracing::field::{Field, Visit};
use tracing::span;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// The exposed name is `carbide_log_events_total`; the Prometheus exporter
/// appends the `_total` itself (the `carbide-instrument` convention).
const INSTRUMENT_NAME: &str = "carbide_log_events";

const LEVELS: [tracing::Level; 5] = [
    tracing::Level::ERROR,
    tracing::Level::WARN,
    tracing::Level::INFO,
    tracing::Level::DEBUG,
    tracing::Level::TRACE,
];

fn level_index(level: &tracing::Level) -> usize {
    LEVELS
        .iter()
        .position(|l| l == level)
        .expect("tracing has exactly five levels")
}

/// Per-component event counts, one atomic per level. Process-global: one
/// process has one truth, so an embedding host's layer (a test harness, a
/// larger binary) and this binary's own registration always meet --
/// [`register`] exposes every counted event, whichever layer counted it.
/// Cardinality is bounded at [`MAX_COMPONENTS`] distinct components.
type Counts = DashMap<String, [AtomicU64; 5]>;

fn global_counts() -> &'static Counts {
    static COUNTS: std::sync::OnceLock<Counts> = std::sync::OnceLock::new();
    COUNTS.get_or_init(Counts::default)
}

/// The counting side of the log-event metric: hand [`LogEventsMetric::layer`]
/// to the subscriber stack at startup, then call [`register`] once the meter
/// provider exists. The type is its own subscriber layer.
#[derive(Debug, Clone)]
pub struct LogEventsMetric {
    default_component: Arc<str>,
}

impl LogEventsMetric {
    /// `default_component` labels events that no enclosing span attributes to
    /// a subsystem -- use the binary's component name (`"nico-api"`, ...).
    pub fn new(default_component: impl Into<Arc<str>>) -> Self {
        Self {
            default_component: default_component.into(),
        }
    }

    /// The subscriber layer that counts every event: the metric type is its
    /// own layer, so this is just a clone for the stack.
    pub fn layer(&self) -> Self {
        self.clone()
    }
}

/// Registers `carbide_log_events_total{level, component}` on `meter`,
/// reporting the cumulative counts since process start. A free function on
/// purpose: the counts are process-wide, so registration needs no instance
/// and no handle threads from logging setup to metrics setup.
pub fn register(meter: &Meter) {
    let counts = global_counts();
    meter
        .u64_observable_counter(INSTRUMENT_NAME)
        .with_description(
            "Number of log events emitted, by level and component. The always-on \
             log-volume and error-rate signal for every binary.",
        )
        .with_callback(move |observer| {
            for entry in counts.iter() {
                for (index, level) in LEVELS.iter().enumerate() {
                    let count = entry.value()[index].load(Ordering::Relaxed);
                    if count > 0 {
                        observer.observe(
                            count,
                            &[
                                KeyValue::new("level", level.as_str()),
                                KeyValue::new("component", entry.key().clone()),
                            ],
                        );
                    }
                }
            }
        })
        .build();
}

/// The most distinct `component` values the counts will hold; events from any
/// further component are counted under [`OVERFLOW_COMPONENT`]. The component
/// label is a runtime string taken from span attributes, so this bound -- far
/// above any real configured component set -- is what keeps a misbehaving span
/// (say, a per-request value recorded under the `component` name) from growing
/// the map and the exported series without limit.
const MAX_COMPONENTS: usize = 64;

/// The catch-all component label once [`MAX_COMPONENTS`] is reached.
const OVERFLOW_COMPONENT: &str = "other";

fn bump(counts: &Counts, component: &str, level: &tracing::Level) {
    let index = level_index(level);
    if let Some(per_level) = counts.get(component) {
        per_level[index].fetch_add(1, Ordering::Relaxed);
        return;
    }
    let key = if counts.len() < MAX_COMPONENTS {
        component
    } else {
        OVERFLOW_COMPONENT
    };
    counts.entry(key.to_string()).or_default()[index].fetch_add(1, Ordering::Relaxed);
}

/// The span attribute the layer resolves the `component` label from, matching
/// the logfmt convention.
struct ComponentTag(String);

impl<S> Layer<S> for LogEventsMetric
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let mut visitor = ComponentVisitor(None);
        attrs.record(&mut visitor);
        if let Some(component) = visitor.0
            && let Some(span) = ctx.span(id)
        {
            span.extensions_mut().insert(ComponentTag(component));
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let level = event.metadata().level();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope {
                if let Some(tag) = span.extensions().get::<ComponentTag>() {
                    bump(global_counts(), &tag.0, level);
                    return;
                }
            }
        }
        bump(global_counts(), &self.default_component, level);
    }
}

struct ComponentVisitor(Option<String>);

impl Visit for ComponentVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "component" {
            self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "component" && self.0.is_none() {
            self.0 = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    fn count(component: &str, level: &tracing::Level) -> u64 {
        global_counts()
            .get(component)
            .map(|per_level| per_level[level_index(level)].load(Ordering::Relaxed))
            .unwrap_or_default()
    }

    #[test]
    fn counts_by_level_with_the_default_component() {
        let metric = LogEventsMetric::new("log-events-test-levels");
        let subscriber = tracing_subscriber::registry().with(metric.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!("boom");
            tracing::warn!("careful");
            tracing::warn!("careful again");
            tracing::info!("hello");
        });

        assert_eq!(count("log-events-test-levels", &tracing::Level::ERROR), 1);
        assert_eq!(count("log-events-test-levels", &tracing::Level::WARN), 2);
        assert_eq!(count("log-events-test-levels", &tracing::Level::INFO), 1);
        assert_eq!(count("log-events-test-levels", &tracing::Level::DEBUG), 0);
    }

    #[test]
    fn component_resolves_from_the_nearest_enclosing_span() {
        let metric = LogEventsMetric::new("log-events-test-spans");
        let subscriber = tracing_subscriber::registry().with(metric.layer());
        tracing::subscriber::with_default(subscriber, || {
            let outer = tracing::info_span!("outer", component = "log-events-test-outer");
            let _outer = outer.enter();
            tracing::warn!("attributed to the subsystem");

            let inner = tracing::info_span!("inner", component = "log-events-test-inner");
            let _inner = inner.enter();
            tracing::warn!("nearest span wins");
        });

        assert_eq!(count("log-events-test-outer", &tracing::Level::WARN), 1);
        assert_eq!(count("log-events-test-inner", &tracing::Level::WARN), 1);
        assert_eq!(count("log-events-test-spans", &tracing::Level::WARN), 0);
    }

    #[test]
    fn components_beyond_the_cap_count_as_other() {
        // A local map, not the process-global one: filling to the cap here
        // must not push the shared counts over it for the other tests.
        let counts = Counts::default();
        for n in 0..MAX_COMPONENTS {
            bump(&counts, &format!("component-{n}"), &tracing::Level::INFO);
        }
        bump(&counts, "one-too-many", &tracing::Level::WARN);
        bump(&counts, "and-another", &tracing::Level::WARN);
        // A component seen before the cap keeps counting under its own name.
        bump(&counts, "component-0", &tracing::Level::INFO);

        assert!(!counts.contains_key("one-too-many"));
        assert!(!counts.contains_key("and-another"));
        let overflow = counts.get(OVERFLOW_COMPONENT).expect("overflow bucket");
        assert_eq!(
            overflow[level_index(&tracing::Level::WARN)].load(Ordering::Relaxed),
            2
        );
        let first = counts.get("component-0").expect("pre-cap component");
        assert_eq!(
            first[level_index(&tracing::Level::INFO)].load(Ordering::Relaxed),
            2
        );
    }

    #[test]
    fn bump_attributes_directly() {
        bump(
            global_counts(),
            "log-events-test-direct",
            &tracing::Level::INFO,
        );
        assert_eq!(count("log-events-test-direct", &tracing::Level::INFO), 1);
    }
}
