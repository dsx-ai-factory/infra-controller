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

use std::cell::RefCell;
use std::marker::PhantomData;

use opentelemetry::metrics::{Counter, Histogram, Meter};
use tracing::{Event, Id, Level, Subscriber, field, span};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

// Returns `Filter` that prevents logs with a level below `WARN` for sqlx
// We use this solely for stdout and OpenTelemetry logging.
// We can't make it a global filter, because our postgres tracing layer requires those logs
#[cfg(test)] // currently only used in tests
pub fn block_sqlx_filter() -> tracing_subscriber::filter::Targets {
    use tracing::metadata::LevelFilter;
    tracing_subscriber::filter::Targets::new()
        .with_default(LevelFilter::TRACE)
        .with_target("sqlx::query", LevelFilter::WARN)
        .with_target("sqlx::extract_query_data", LevelFilter::WARN)
}

pub const SQLX_STATEMENTS_LOG_LEVEL: Level = Level::INFO;

/// Creates a tracing `Layer` which intercepts `sqlx::query` calls only and aggregates their data
pub fn create_sqlx_query_tracing_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    SqlxQueryTracingLayer::default()
        .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
            metadata.is_span()
                || metadata.is_event()
                    && (metadata.target() == "sqlx::query"
                        || metadata.target() == "sqlx::extract_query_data")
        }))
        .with_filter(LevelFilter::from_level(SQLX_STATEMENTS_LOG_LEVEL))
}

/// A tracing [Layer] that listens to `sqlx::query` events
///
/// sqlx emits a `tracing` event with `target: sqlx::query` for every query that
/// is performed:
/// ```ignore
/// private_tracing_dynamic_event!(
///     target: "sqlx::query",
///     tracing_level,
///     summary,
///     db.statement = sql,
///     rows_affected = self.rows_affected, // u64 in Debug format
///     rows_returned = self.rows_returned, // u64 in Debug format
///     ?elapsed,                           // Duration in Debug format
/// );
/// ```
/// See https://github.com/launchbadge/sqlx/blob/7e7dded8afd93fd74d6d6f65cd9187fea78b4d0f/sqlx-core/src/logger.rs#L117-L125
///
/// We intercept this event, and carry over the most important data into our own
/// request logs.
struct SqlxQueryTracingLayer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    _s: PhantomData<S>,
}

#[derive(Debug, Clone, Default)]
pub struct SqlxQueryDataAggregation {
    pub num_queries: usize,
    pub total_rows_affected: usize,
    pub total_rows_returned: usize,
    pub max_query_duration: std::time::Duration,
    /// The query which had the highest execution time
    pub max_query_duration_summary: String,
    pub total_query_duration: std::time::Duration,
}

impl SqlxQueryDataAggregation {
    fn aggregate(&mut self, query_data: &SqlxQueryDataExtractor) {
        self.num_queries += 1;
        self.total_rows_affected += query_data.rows_affected;
        self.total_rows_returned += query_data.rows_returned;
        self.total_query_duration = self.total_query_duration.saturating_add(query_data.elapsed);
        if query_data.elapsed > self.max_query_duration {
            self.max_query_duration = query_data.elapsed;
            self.max_query_duration_summary = query_data.db_statement.clone();
        }
    }

    /// Calculates the diff between the most recent query data aggregation (`self`)
    /// and a previous query data aggregation. This diff contains information
    /// about the queries between the previous and most recent aggregation.
    ///
    /// The diff can not provide accurate information about the longest
    /// query (`max_query_duration`).
    pub fn diff(&self, previous: &SqlxQueryDataAggregation) -> Self {
        Self {
            num_queries: self.num_queries.saturating_sub(previous.num_queries),
            total_rows_affected: self
                .total_rows_affected
                .saturating_sub(previous.total_rows_affected),
            total_rows_returned: self
                .total_rows_returned
                .saturating_sub(previous.total_rows_returned),
            max_query_duration: self.max_query_duration,
            max_query_duration_summary: self.max_query_duration_summary.clone(),
            total_query_duration: self
                .total_query_duration
                .saturating_sub(previous.total_query_duration),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct SqlxQueryDataExtractor {
    db_statement: String,
    rows_affected: usize,
    rows_returned: usize,
    elapsed: std::time::Duration,
}

impl field::Visit for SqlxQueryDataExtractor {
    fn record_bool(&mut self, _field: &field::Field, _value: bool) {}

    fn record_f64(&mut self, _field: &field::Field, _value: f64) {}

    fn record_i64(&mut self, _field: &field::Field, _value: i64) {}

    fn record_str(&mut self, field: &field::Field, value: &str) {
        if field.name() == "db.statement" {
            self.db_statement = truncate(
                value
                    .trim()
                    .lines()
                    .map(|l| l.trim())
                    .collect::<Vec<&str>>()
                    .join(" "),
                150,
            )
        }
    }

    fn record_debug(&mut self, field: &field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "elapsed" => {
                // TODO: This would be so much easier if sqlx would give us not a Debug object
                // but just some value
                let mut elapsed_str = format!("{value:?}");
                if elapsed_str.ends_with("ns") {
                    elapsed_str.truncate(elapsed_str.len() - 2);
                    self.elapsed = std::time::Duration::from_nanos(
                        elapsed_str.parse::<f64>().unwrap_or_default() as u64,
                    );
                } else if elapsed_str.ends_with("µs") {
                    elapsed_str.truncate(elapsed_str.len() - "µs".len()); // Yay UTF8
                    self.elapsed = std::time::Duration::from_micros(
                        elapsed_str.parse::<f64>().unwrap_or_default() as u64,
                    );
                } else if elapsed_str.ends_with("ms") {
                    elapsed_str.truncate(elapsed_str.len() - 2);
                    self.elapsed = std::time::Duration::from_millis(
                        elapsed_str.parse::<f64>().unwrap_or_default() as u64,
                    );
                } else if elapsed_str.ends_with('s') {
                    elapsed_str.truncate(elapsed_str.len() - 1);
                    self.elapsed = std::time::Duration::from_secs_f64(
                        elapsed_str.parse::<f64>().unwrap_or_default(),
                    );
                } else {
                    // This should only happen when we upgrade Rust and the std::time::Duration `Debug` impl would get changed
                    panic!("Unhandled time unit");
                }
            }
            "rows_affected" => {
                self.rows_affected = format!("{value:?}").parse::<usize>().unwrap_or_default()
            }
            "rows_returned" => {
                self.rows_returned = format!("{value:?}").parse::<usize>().unwrap_or_default()
            }
            _ => {}
        }
    }

    fn record_error(&mut self, _field: &field::Field, _value: &(dyn std::error::Error + 'static)) {}
}

impl<S> Default for SqlxQueryTracingLayer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn default() -> Self {
        Self { _s: PhantomData }
    }
}

impl<S> Layer<S> for SqlxQueryTracingLayer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_new_span(&self, _attrs: &span::Attributes<'_>, _id: &Id, _ctx: Context<'_, S>) {
        // We could add `SqlxQueryDataAggregation` to each span here.
        // However we currently prefer to just attach it to spans on demand when there
        // is an actual query
    }

    fn on_enter(&self, _id: &Id, _ctx: Context<'_, S>) {}

    fn on_exit(&self, _id: &Id, _ctx: Context<'_, S>) {
        // This is just a temporary exit from the Span and it might be re-entered.
        // We don't care about it. The interesting things happen in `on_close()`
    }

    fn on_record(&self, _id: &Id, _values: &span::Record<'_>, _ctx: Context<'_, S>) {}

    fn on_follows_from(&self, _id: &Id, _follows: &Id, _ctx: Context<S>) {}

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if event.metadata().target() == "sqlx::extract_query_data" {
            // This is a "magic" event that asks the layer to update the thread-local
            // QUERY_DATA variable with all data that had been collected so far.
            if let Some(span) = ctx.lookup_current() {
                let extensions = span.extensions();
                if let Some(span_query_data) = extensions.get::<SqlxQueryDataAggregation>() {
                    QUERY_DATA
                        .with(|data_store| *data_store.borrow_mut() = span_query_data.clone());
                }
            }
            return;
        } else if event.metadata().target() != "sqlx::query" {
            return;
        }

        // Here we are handling the `sqlx::query` event which is used by the sqlx library
        // to emit tracing data

        // We first intercept and parse data:
        let mut query_data = SqlxQueryDataExtractor::default();
        event.record(&mut query_data);

        // Then we use the extracted data to update our aggregated information.
        // This information is stored inside the span
        if let Some(span) = ctx.lookup_current() {
            let mut extensions = span.extensions_mut();
            let span_query_data = match extensions.get_mut::<SqlxQueryDataAggregation>() {
                Some(data) => data,
                None => {
                    extensions.insert(SqlxQueryDataAggregation::default());
                    extensions
                        .get_mut::<SqlxQueryDataAggregation>()
                        .expect("Just inserted")
                }
            };
            span_query_data.aggregate(&query_data);
        }
    }

    fn on_close(&self, _id: Id, _ctx: Context<'_, S>) {
        // This is the final exit from the Span
        // We are however unable to patch attributes in it. Therefore some other code
        // needs to manually do this before the span is town down.
        // The `fetch_and_update_current_span_attributes` can be used to achieve this goal
    }
}

thread_local! {
    /// Thread-local cache where we park aggregated query data in order to transfer
    /// it between the `SqlxQueryTracingLayer` and a consumer.
    pub static QUERY_DATA: RefCell<SqlxQueryDataAggregation> = RefCell::new(SqlxQueryDataAggregation::default());
}

pub fn fetch_and_update_current_span_attributes() -> SqlxQueryDataAggregation {
    let span = tracing::Span::current();
    // The magic event will make an attached `SqlxQueryTracingLayer` update `QUERY_DATA` for us
    tracing::event!(target: "sqlx::extract_query_data", tracing::Level::INFO, {});
    // We now grab the data and patch our span attributes
    let query_data = QUERY_DATA.with(|data| data.take());
    span.record("sql_queries", query_data.num_queries);
    span.record("sql_total_rows_affected", query_data.total_rows_affected);
    span.record("sql_total_rows_returned", query_data.total_rows_returned);
    span.record(
        "sql_max_query_duration_us",
        query_data.max_query_duration.as_micros(),
    );
    span.record(
        "sql_max_query_duration_summary",
        &query_data.max_query_duration_summary,
    );
    span.record(
        "sql_total_query_duration_us",
        query_data.total_query_duration.as_micros(),
    );
    query_data
}

#[derive(Debug, Clone)]
pub struct DatabaseMetricEmitters {
    db_queries_counter: Counter<u64>,
    db_span_query_times: Histogram<f64>,
}

impl DatabaseMetricEmitters {
    pub fn new(meter: &Meter) -> Self {
        // Loosely modelled after
        // https://github.com/open-telemetry/semantic-conventions/tree/main/docs/database
        // The exact operation that we care about - which is just the query time - isn't modelled there

        // Note that this counter does not equal the _count of `carbide-api.db.total_query_time.ms`
        // The reason for this is the `carbide-api.db.total_query_time.ms` will only be incremented once
        // per span. It thereby counts the amount of spans - not the amount of DB queries.
        let db_queries_counter = meter
            .u64_counter("carbide-api.db.queries")
            .with_description("Number of database queries that occurred inside a span")
            .build();

        let db_span_query_times = meter
            .f64_histogram("carbide-api.db.span_query_time")
            .with_description("Total time the request spent inside a span on database transactions")
            .with_unit("ms")
            .build();

        Self {
            db_span_query_times,
            db_queries_counter,
        }
    }

    /// Emits database performance records that have been aggregated inside a Span
    pub fn emit(&self, metrics: &SqlxQueryDataAggregation, attributes: &[opentelemetry::KeyValue]) {
        self.db_queries_counter
            .add(metrics.num_queries as u64, attributes);

        self.db_span_query_times.record(
            metrics.total_query_duration.as_secs_f64() * 1000.0,
            attributes,
        );
    }
}

fn truncate(mut s: String, max_chars: usize) -> String {
    if s.len() <= max_chars {
        // shortcut for ascii that's already short enough - 99%+ of calls
        return s;
    }
    let (idx, _) = s.char_indices().nth(max_chars).unwrap();
    s.truncate(idx);
    s += "...";
    s
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::prelude::*;

    use super::*;

    struct MutexWriter {
        writer: Mutex<Vec<u8>>,
    }

    impl MutexWriter {
        fn lines(&self) -> Vec<String> {
            let guard = self.writer.lock().unwrap();
            let data = std::str::from_utf8(&guard).unwrap();
            data.lines()
                .map(|line| line.to_string())
                .collect::<Vec<String>>()
        }
    }

    impl std::io::Write for &MutexWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut guard = self.writer.lock().unwrap();
            guard.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            let mut guard = self.writer.lock().unwrap();
            guard.flush()
        }
    }

    #[test]
    fn test_sqlx_subscriber() {
        use std::time::Duration;

        use tracing::metadata::{Level, LevelFilter};

        let writer = Arc::new(MutexWriter {
            writer: Mutex::new(Vec::new()),
        });

        let fmt_layer = tracing_subscriber::fmt::layer()
            .without_time()
            .compact()
            .with_ansi(false)
            .with_writer(writer.clone());

        let _subscriber = tracing_subscriber::registry()
            .with(create_sqlx_query_tracing_layer())
            .with(fmt_layer.with_filter(block_sqlx_filter()))
            .with(LevelFilter::from_level(Level::INFO))
            .set_default();

        {
            let span = tracing::span!(
                parent: None,
                tracing::Level::WARN,
                "sqlx_test_span",
                sql_queries = 0,
                sql_total_rows_affected = 0,
                sql_total_rows_returned = 0,
                sql_max_query_duration_us = 0,
                sql_max_query_duration_summary = tracing::field::Empty,
                sql_total_query_duration_us = 0,
            );

            let _entered = span.enter();
            tracing::event!(target: "sqlx::query", Level::INFO, summary = "Summary1", db.statement = "Statement1", rows_affected = 2u64, rows_returned = 1u64, elapsed = ?Duration::from_nanos(111));
            tracing::event!(target: "sqlx::query", Level::INFO, summary = "Summary2", db.statement = "Statement2", rows_affected = 4u64, rows_returned = 3u64, elapsed = ?Duration::from_micros(211));
            tracing::event!(target: "sqlx::query", Level::INFO, summary = "Summary3", db.statement = "Statement3", rows_affected = 8u64, rows_returned = 6u64, elapsed = ?Duration::from_millis(311));
            tracing::event!(target: "sqlx::query", Level::INFO, summary = "Summary4", db.statement = "Statement4", rows_affected = 16u64, rows_returned = 9u64, elapsed = ?Duration::from_secs(411));

            fetch_and_update_current_span_attributes();

            // The default formatter will only log attributs on events. Therefore we produce one
            // Note that it will also log the initial values of attributes, e.g. sql_queries = 0
            // This is however a problem of that formatter, and not of our logic.
            // Other formatters - like the JSON and opentelemetry formatters - get it right.
            tracing::info!("Log the attributes");
            drop(_entered);

            let log_lines = writer.lines();
            assert_eq!(
                log_lines.len(),
                1,
                "Expected just one log line since we suppress intermediate queries"
            );

            assert!(log_lines[0].contains("sql_queries=4"));
            assert!(log_lines[0].contains("sql_total_rows_affected=30"));
            assert!(log_lines[0].contains("sql_total_rows_returned=19"));
            assert!(log_lines[0].contains("sql_max_query_duration_us=411000000"));
            assert!(log_lines[0].contains("sql_total_query_duration_us=411311211"));
            // We use the first 150 chars of the statement as the summary for more detail
            assert!(log_lines[0].contains("sql_max_query_duration_summary=\"Statement4\""));
        }
    }
}
