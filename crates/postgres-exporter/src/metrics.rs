// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Meter, ObservableGauge};

#[derive(sqlx::FromRow)]
pub(super) struct Statement {
    pub(super) statement_id: String,
    pub(super) query: String,
    pub(super) calls: i64,
    pub(super) sec_total: f64,
    pub(super) ms_avg: f64,
}

#[derive(Default)]
struct State {
    up: AtomicU64,
    last_success: AtomicU64,
    statements: Mutex<Option<Vec<Statement>>>,
}

pub(super) struct Metrics {
    state: Arc<State>,
    _up: ObservableGauge<u64>,
    _last_success: ObservableGauge<u64>,
    _statements_up: ObservableGauge<u64>,
    _statement_calls: ObservableGauge<u64>,
    _statement_total: ObservableGauge<f64>,
    _statement_mean: ObservableGauge<f64>,
}

impl Metrics {
    pub(super) fn new(meter: &Meter) -> Self {
        let state = Arc::new(State::default());
        let up_state = state.clone();
        let success_state = state.clone();
        let statements_up_state = state.clone();
        let calls_state = state.clone();
        let total_state = state.clone();
        let mean_state = state.clone();
        Self {
            state,
            _up: meter.u64_observable_gauge("nico_postgres_exporter_database_up")
                .with_description("1 if the latest PostgreSQL hello-world query succeeded; 0 before the first success, on failure, or on timeout.")
                .with_callback(move |observer| observer.observe(up_state.up.load(Ordering::Relaxed), &[]))
                .build(),
            _last_success: meter.u64_observable_gauge("nico_postgres_exporter_last_success_timestamp_seconds")
                .with_description("Unix timestamp in seconds of the latest successful PostgreSQL hello-world query; 0 until a query succeeds. Failures preserve this timestamp.")
                .with_callback(move |observer| observer.observe(success_state.last_success.load(Ordering::Relaxed), &[]))
                .build(),
            _statements_up: meter.u64_observable_gauge("nico_postgres_exporter_stat_statements_up")
                .with_description("1 if the latest pg_stat_statements collection succeeded; 0 before collection, on failure, or on timeout.")
                .with_callback(move |observer| observer.observe(u64::from(statements_up_state.statements.lock().expect("statement snapshot mutex poisoned").is_some()), &[]))
                .build(),
            _statement_calls: meter.u64_observable_gauge("nico_postgres_statement_calls")
                .with_description("Cumulative calls from pg_stat_statements for each of the top 20 statements by total execution time. Snapshot gauge; resets with PostgreSQL statistics.")
                .with_callback(move |observer| {
                    let snapshot = calls_state.statements.lock().expect("statement snapshot mutex poisoned");
                    for statement in snapshot.iter().flatten() {
                        observer.observe(statement.calls as u64, &statement.labels());
                    }
                })
                .build(),
            _statement_total: meter.f64_observable_gauge("nico_postgres_statement_exec_seconds")
                .with_description("Cumulative execution time in seconds from pg_stat_statements for each of the top 20 statements by total execution time. Snapshot gauge; resets with PostgreSQL statistics.")
                .with_callback(move |observer| {
                    let snapshot = total_state.statements.lock().expect("statement snapshot mutex poisoned");
                    for statement in snapshot.iter().flatten() {
                        observer.observe(statement.sec_total, &statement.labels());
                    }
                })
                .build(),
            _statement_mean: meter.f64_observable_gauge("nico_postgres_statement_mean_exec_milliseconds")
                .with_description("Mean execution time in milliseconds from pg_stat_statements for each of the top 20 statements by total execution time.")
                .with_callback(move |observer| {
                    let snapshot = mean_state.statements.lock().expect("statement snapshot mutex poisoned");
                    for statement in snapshot.iter().flatten() {
                        observer.observe(statement.ms_avg, &statement.labels());
                    }
                })
                .build(),
        }
    }

    pub(super) fn record_statements(&self, statements: Option<Vec<Statement>>) {
        *self
            .state
            .statements
            .lock()
            .expect("statement snapshot mutex poisoned") = statements;
    }

    pub(super) fn record(&self, success: bool) {
        self.state.up.store(u64::from(success), Ordering::Relaxed);
        if success {
            self.state.last_success.store(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                Ordering::Relaxed,
            );
        }
    }
}

impl Statement {
    fn labels(&self) -> [KeyValue; 2] {
        // Text alone is not unique across databases, roles, and top-level/nested execution.
        [
            KeyValue::new("statement_id", self.statement_id.clone()),
            KeyValue::new("query", self.query.clone()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_initial_failure_and_recovery_state() {
        let setup =
            metrics_endpoint::new_metrics_setup("postgres-exporter", "test", false).unwrap();
        let metrics = Metrics::new(&setup.meter);
        let up = "nico_postgres_exporter_database_up";
        let success = "nico_postgres_exporter_last_success_timestamp_seconds";
        let value = |name: &str| {
            setup
                .registry
                .gather()
                .into_iter()
                .find(|family| family.name() == name)
                .unwrap()
                .get_metric()[0]
                .get_gauge()
                .value()
        };
        assert_eq!(value(up), 0.0);
        assert_eq!(value(success), 0.0);
        metrics.record(true);
        let timestamp = value(success);
        assert!(timestamp > 0.0);
        metrics.record(false);
        assert_eq!(value(up), 0.0);
        assert_eq!(value(success), timestamp);
        metrics.record(true);
        assert_eq!(value(up), 1.0);
        assert!(value(success) >= timestamp);
        let text = prometheus::TextEncoder::new()
            .encode_to_string(&setup.registry.gather())
            .unwrap();
        assert!(text.contains(&format!("# TYPE {up} gauge")));
        assert!(text.contains(&format!("# TYPE {success} gauge")));
    }
}
