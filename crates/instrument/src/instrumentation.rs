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

use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};

use crate::__private::MetricKey;
use crate::{Event, LogAt, MetricKind};

/// A metric destination and its cache of declared Event instruments.
///
/// Clones share the meter and cache. Independent providers and registries are
/// required for isolation: constructing two handles from the same provider
/// still records into the same destination. The owner must retain the meter
/// provider for the lifetime of all recording tasks and scrapes.
///
/// This handle routes Event metrics only. The RED helper and log-event counts
/// remain process-global; observable instruments remain owned by their meter.
#[derive(Debug, Clone)]
pub struct Instrumentation {
    source: Source,
}

#[derive(Debug, Clone)]
enum Source {
    Global,
    Owned(Arc<Instruments>),
}

#[derive(Debug)]
struct Instruments {
    meter: Meter,
    // Identity belongs to the declaration, never to a runtime label set.
    cache: DashMap<MetricKey, CachedInstrument>,
}

impl Instrumentation {
    /// Creates an instrument cache using the supplied meter, including its
    /// provider's existing exporters, resource attributes, and aggregations.
    /// This does not install or change the global OpenTelemetry provider.
    pub fn new(meter: Meter) -> Self {
        Self {
            source: Source::Owned(Arc::new(Instruments {
                meter,
                cache: DashMap::new(),
            })),
        }
    }

    /// Returns a lazy alias for the process-global compatibility destination.
    ///
    /// All such handles and the free recording functions share one cache.
    /// Constructing or cloning a handle does not access the global provider;
    /// the first metric recording (including counter-series initialization)
    /// selects the global meter. Install the provider before that recording.
    /// Replacing the provider afterward does not redirect cached instruments.
    /// Log-only events do not initialize the destination.
    pub fn global() -> Self {
        Self {
            source: Source::Global,
        }
    }

    /// Emits one event's optional log and metric into this destination.
    ///
    /// Counts one occurrence for counters and records the declared observation
    /// for histograms and gauges. Logging uses the event's own log control and
    /// the current tracing subscriber, independently of the metric destination.
    pub fn emit<E: Event>(&self, event: E) {
        if let LogAt::Level(level) = event.log_at() {
            event.__log(level);
        }
        match self.instrument::<E>() {
            CachedInstrument::Counter(counter) => counter.add(1, event.labels().as_ref()),
            CachedInstrument::Histogram(histogram) => {
                histogram.record(event.observation(), event.labels().as_ref());
            }
            CachedInstrument::Gauge(gauge) => {
                gauge.record(event.observation(), event.labels().as_ref());
            }
            CachedInstrument::None => {}
        }
    }

    /// Emits one counter event with an explicit delta into this destination.
    ///
    /// Writes at most one log line, even for a zero delta. Non-counter events
    /// retain their logging behavior but do not register or record a metric.
    pub fn emit_count<E: Event>(&self, event: E, count: u64) {
        if let LogAt::Level(level) = event.log_at() {
            event.__log(level);
        }
        if matches!(E::METRIC, MetricKind::Counter)
            && let CachedInstrument::Counter(counter) = self.instrument::<E>()
        {
            counter.add(count, event.labels().as_ref());
        }
    }

    /// Exposes this counter's bounded label set at zero without logging an
    /// occurrence. Subsequent emissions reuse the same cached instrument.
    /// Context fields are ignored.
    ///
    /// Returns `false` without registering anything for non-counter events.
    #[must_use = "a false result means the Event is not a counter"]
    pub fn initialize_counter_series<E: Event>(&self, event: &E) -> bool {
        if matches!(E::METRIC, MetricKind::Counter)
            && let CachedInstrument::Counter(counter) = self.instrument::<E>()
        {
            counter.add(0, event.labels().as_ref());
            return true;
        }
        false
    }

    fn instruments(&self) -> &Instruments {
        match &self.source {
            Source::Owned(instruments) => instruments,
            Source::Global => {
                static GLOBAL: OnceLock<Instruments> = OnceLock::new();
                GLOBAL.get_or_init(|| Instruments {
                    meter: opentelemetry::global::meter("carbide-instrument"),
                    cache: DashMap::new(),
                })
            }
        }
    }

    fn instrument<E: Event>(&self) -> CachedInstrument {
        if matches!(E::METRIC, MetricKind::None) {
            return CachedInstrument::None;
        }
        let instruments = self.instruments();
        let key = E::__metric_key();
        if let Some(instrument) = instruments.cache.get(&key) {
            return instrument.clone();
        }
        instruments
            .cache
            .entry(key)
            .or_insert_with(|| {
                CachedInstrument::new(&instruments.meter, E::METRIC_NAME, E::METRIC, E::DESCRIBE)
            })
            .clone()
    }
}

#[derive(Debug, Clone)]
enum CachedInstrument {
    Counter(Counter<u64>),
    Histogram(Histogram<f64>),
    Gauge(Gauge<f64>),
    None,
}

impl CachedInstrument {
    // Names here are the exposed names. The exporter appends conventional
    // suffixes, so strip them when registering to preserve exported series.
    fn new(
        meter: &Meter,
        metric_name: Option<&'static str>,
        metric: MetricKind,
        describe: &'static str,
    ) -> Self {
        let Some(metric_name) = metric_name else {
            return Self::None;
        };
        match metric {
            MetricKind::Counter => {
                let name = metric_name.strip_suffix("_total").unwrap_or(metric_name);
                let mut builder = meter.u64_counter(name);
                if !describe.is_empty() {
                    builder = builder.with_description(describe);
                }
                Self::Counter(builder.build())
            }
            MetricKind::Histogram { unit } => {
                let name = strip_unit_suffix(metric_name, unit);
                let mut builder = meter.f64_histogram(name);
                if !unit.is_empty() {
                    builder = builder.with_unit(unit);
                }
                if !describe.is_empty() {
                    builder = builder.with_description(describe);
                }
                Self::Histogram(builder.build())
            }
            MetricKind::Gauge { unit } => {
                let name = strip_unit_suffix(metric_name, unit);
                let mut builder = meter.f64_gauge(name);
                if !unit.is_empty() {
                    builder = builder.with_unit(unit);
                }
                if !describe.is_empty() {
                    builder = builder.with_description(describe);
                }
                Self::Gauge(builder.build())
            }
            MetricKind::None => Self::None,
        }
    }
}

fn strip_unit_suffix(metric_name: &'static str, unit: &'static str) -> &'static str {
    let suffix = match unit {
        "s" => "_seconds",
        "ms" => "_milliseconds",
        "us" => "_microseconds",
        "By" => "_bytes",
        _ => "",
    };
    if suffix.is_empty() {
        metric_name
    } else {
        metric_name.strip_suffix(suffix).unwrap_or(metric_name)
    }
}
