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

use std::collections::HashMap;
use std::sync::Arc;

use measured_boot::journal::MeasurementJournal;
use measured_boot::records::MeasurementBundleState;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::CarbideResult;
use crate::cfg::file::MeasuredBootMetricsCollectorConfig;

pub(in crate::measured_boot) mod metrics;
use carbide_uuid::measured_boot::MeasurementBundleId;
use metrics::MeasuredBootMetricsCollectorMetrics;

/// `MeasuredBootCollectorIteration` records one full collector pass over
/// profiles, bundles, and machines. Its histogram preserves both latency and
/// the per-outcome iteration count. Successful passes stay metric-only; a
/// failed pass also writes the collector warning with its error context.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "measured_boot_collector_iteration",
    metric_name = "carbide_measured_boot_collector_iteration_latency_milliseconds",
    component = "nico-api",
    log = dynamic,
    metric = histogram,
    message = "MeasuredBootMetricsCollector error",
    describe = "Number of milliseconds a full measured boot metrics collector iteration took, by outcome"
)]
struct MeasuredBootCollectorIteration {
    #[label]
    outcome: carbide_instrument::Outcome,
    #[observation]
    took: std::time::Duration,
    /// `error` carries failure detail. Successful passes use `""` because
    /// their generated log is disabled while the histogram still records them.
    #[context]
    error: String,
}

impl carbide_instrument::DynamicLog for MeasuredBootCollectorIteration {
    fn log_at(&self) -> carbide_instrument::LogAt {
        match self.outcome {
            carbide_instrument::Outcome::Ok => carbide_instrument::LogAt::Off,
            carbide_instrument::Outcome::Error => {
                carbide_instrument::LogAt::Level(tracing::Level::WARN)
            }
        }
    }
}

/// `MeasuredBootMetricsCollector` monitors the state of all measured boot data.
pub(crate) struct MeasuredBootMetricsCollector {
    database_connection: sqlx::PgPool,
    config: MeasuredBootMetricsCollectorConfig,
    metric_holder: Arc<metrics::MetricHolder>,
}

impl MeasuredBootMetricsCollector {
    /// Create a MeasuredBootMetricsCollector
    pub(crate) fn new(
        database_connection: sqlx::PgPool,
        config: MeasuredBootMetricsCollectorConfig,
        meter: opentelemetry::metrics::Meter,
    ) -> Self {
        // We want to hold metrics for longer than the iteration interval, so there is continuity
        // in emitting metrics. However we want to avoid reporting outdated metrics in case
        // reporting gets stuck. Therefore round up the iteration interval by 1min.
        let hold_period = config
            .run_interval
            .saturating_add(std::time::Duration::from_secs(60));

        let metric_holder = Arc::new(metrics::MetricHolder::new(meter, hold_period));

        MeasuredBootMetricsCollector {
            database_connection,
            config,
            metric_holder,
        }
    }

    /// Start the MeasuredBootMetricsCollector and return a [sending channel](tokio::sync::oneshot::Sender)
    /// that will stop the MeasuredBootMetricsCollector when dropped.
    pub(crate) fn start(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> std::io::Result<()> {
        if self.config.enabled {
            join_set
                .build_task()
                .name("measured_boot_collector")
                .spawn(async move { self.run(cancel_token).await })?;
        }

        Ok(())
    }

    async fn run(&self, cancel_token: CancellationToken) {
        loop {
            // `run_single_iteration` emits a failure before returning it, so
            // this loop can discard the result and keep scheduling later
            // passes.
            let _ = self.run_single_iteration().await;

            tokio::select! {
                _ = tokio::time::sleep(self.config.run_interval) => {},
                _ = cancel_token.cancelled() => {
                    tracing::info!("MeasuredBootMetricsCollector stop was requested");
                    return;
                }
            }
        }
    }

    pub(in crate::measured_boot) async fn run_single_iteration(&self) -> CarbideResult<()> {
        let started = std::time::Instant::now();
        let result = self.collect_metrics().await;
        carbide_instrument::emit(MeasuredBootCollectorIteration {
            outcome: carbide_instrument::Outcome::from(&result),
            took: started.elapsed(),
            error: match &result {
                Ok(()) => String::new(),
                Err(error) => error.to_string(),
            },
        });
        result
    }

    async fn collect_metrics(&self) -> CarbideResult<()> {
        let mut metrics = MeasuredBootMetricsCollectorMetrics::new();

        let mut txn = db::Transaction::begin(&self.database_connection).await?;

        let profiles = db::measured_boot::profile::get_all(&mut txn).await?;
        for system_profile in profiles.iter() {
            let machines =
                db::measured_boot::profile::get_machines(system_profile, &mut txn).await?;
            metrics
                .num_machines_per_profile
                .insert(system_profile.profile_id, machines.len());
        }
        metrics.num_profiles = profiles.len();

        let bundles = db::measured_boot::bundle::get_all(&mut txn).await?;
        let bundle_map: HashMap<MeasurementBundleId, MeasurementBundleState> = bundles
            .iter()
            .map(|bundle| (bundle.bundle_id, bundle.state))
            .collect();

        for bundle in bundles.iter() {
            let machines = db::measured_boot::bundle::get_machines(bundle, &mut txn).await?;
            metrics
                .num_machines_per_bundle
                .insert(bundle.bundle_id, machines.len());
            for pcr_register_value in bundle.pcr_values().into_iter() {
                *metrics
                    .num_machines_per_pcr_value
                    .entry(pcr_register_value)
                    .or_insert(0) += 1;
            }
        }
        metrics.num_bundles = bundles.len();

        let machines = db::measured_boot::machine::get_all(&mut txn).await?;
        for machine in machines.iter() {
            let bundle_state = get_bundle_state(&bundle_map, &machine.journal);
            *metrics
                .num_machines_per_machine_state
                .entry(machine.state)
                .or_insert(0) += 1;
            *metrics
                .num_machines_per_bundle_state
                .entry(bundle_state)
                .or_insert(0) += 1;
        }
        metrics.num_machines = machines.len();

        // Cache all other metrics that have been captured in this iteration.
        // Those will be queried by OTEL on demand
        self.metric_holder.update_metrics(metrics);

        txn.commit().await?;

        Ok(())
    }
}

/// get_bundle_state attempts to get the bundle state for a given
/// journal and complete map of currently known bundle IDs and their
/// given states.
///
/// TODO(chet): This exists because machines don't have a bundle state
/// stored alongside them yet, because we don't store a bundle state in
/// the journal entry (just the bundle ID). Going and fetching the bundle
/// state for each machine would be expensive, so for now, this works, but
/// look into storing an Option<MeasurementBundleState> in the journal
/// at entry time.
///
/// TODO(chet): Introduce a new state here that isn't ::Pending for cases
/// where there is no bundle match at all -- ::Pending means the bundle
/// exists but isn't active/revoked yet. When there is no bundle match,
/// the state should be ::NoMatch or something similar.
fn get_bundle_state(
    bundle_map: &HashMap<MeasurementBundleId, MeasurementBundleState>,
    machine_journal: &Option<MeasurementJournal>,
) -> MeasurementBundleState {
    if let Some(journal) = machine_journal {
        if let Some(bundle_id) = journal.bundle_id {
            if let Some(bundle_state) = bundle_map.get(&bundle_id) {
                *bundle_state
            } else {
                MeasurementBundleState::Pending
            }
        } else {
            MeasurementBundleState::Pending
        }
    } else {
        MeasurementBundleState::Pending
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{ApproxHistogramSum, MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};

    use super::*;

    /// `MeasuredBootCollectorIteration` always records latency under its
    /// existing `outcome` label. Successful passes stay silent; a failure also
    /// writes the collector's WARN record with `error` context.
    #[test]
    fn collector_iteration_logs_failures_and_records_latency() {
        struct IterationCase {
            outcome: carbide_instrument::Outcome,
            outcome_label: &'static str,
            milliseconds: u64,
            error: &'static str,
        }

        #[derive(Debug, PartialEq)]
        struct LogObservation {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            outcome: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            log_count: usize,
            log: Option<LogObservation>,
            histogram_count_delta: u64,
            histogram_sum_delta: ApproxHistogramSum,
        }

        const METRIC_NAME: &str = "carbide_measured_boot_collector_iteration_latency_milliseconds";

        check_values(
            [
                Check {
                    scenario: "successful iteration",
                    input: IterationCase {
                        outcome: carbide_instrument::Outcome::Ok,
                        outcome_label: "ok",
                        milliseconds: 1500,
                        error: "",
                    },
                    expect: Observation {
                        log_count: 0,
                        log: None,
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(1500.0),
                    },
                },
                Check {
                    scenario: "failed iteration",
                    input: IterationCase {
                        outcome: carbide_instrument::Outcome::Error,
                        outcome_label: "error",
                        milliseconds: 250,
                        error: "database unavailable",
                    },
                    expect: Observation {
                        log_count: 1,
                        log: Some(LogObservation {
                            level: tracing::Level::WARN,
                            metadata_name: "measured_boot_collector_iteration".to_string(),
                            message: "MeasuredBootMetricsCollector error".to_string(),
                            event_name: Some("measured_boot_collector_iteration".to_string()),
                            metric_name: Some(METRIC_NAME.to_string()),
                            outcome: Some("error".to_string()),
                            error: Some("database unavailable".to_string()),
                        }),
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(250.0),
                    },
                },
            ],
            |IterationCase {
                 outcome,
                 outcome_label,
                 milliseconds,
                 error,
             }| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    carbide_instrument::emit(MeasuredBootCollectorIteration {
                        outcome,
                        took: std::time::Duration::from_millis(milliseconds),
                        error: error.to_string(),
                    });
                });
                let log = logs.first().map(|log| LogObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    outcome: log.field("outcome").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                });

                Observation {
                    log_count: logs.len(),
                    log,
                    histogram_count_delta: metrics
                        .histogram_count_delta(METRIC_NAME, &[("outcome", outcome_label)]),
                    histogram_sum_delta: metrics
                        .histogram_sum_delta(METRIC_NAME, &[("outcome", outcome_label)]),
                }
            },
        );
    }
}
