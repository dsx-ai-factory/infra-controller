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

//! Gauges for the DPF maintenance holds that are still outstanding.
//!
//! DPF parks a host's DPUs behind a maintenance hold while a changed DPUService
//! waits to roll out, and carbide records the hold as an outstanding
//! `DpuServiceSync` pending action. The state handler reads that marker only
//! from `Ready`, and the operator worklist is the only other view of it, so a
//! host that was both held and stuck did not show up in monitoring. These
//! gauges cover the held hosts in every state.
//!
//! They describe rather than diagnose. A held DPU keeps serving its host, so a
//! held host that is past its SLA is usually stuck for some other reason. The
//! labels let a rule single such hosts out without claiming the hold is why.
//!
//! The hosts counted are the worklist `dpf service-sync list` prints: every
//! outstanding row, whatever the host's `used_for_ingestion` flag and the
//! site's `dpu_service_sync_enabled` switch say about who may release it. A
//! site without DPF never writes a row, so it reports zero.

use std::io;
use std::time::Duration;

use carbide_utils::metrics::SharedMetricsHolder;
use carbide_utils::periodic_timer::PeriodicTimer;
use chrono::{DateTime, Utc};
use db::DbSessionGuard;
use health_report::HealthReport;
use model::machine::slas::MachineSlaConfig;
use model::machine::{HostHealthConfig, state_sla};
use model::machine_pending_action::MachinePendingActionKind::DpuServiceSync;
use model::machine_pending_action::PendingActionHost;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::DatabaseError;

/// How often the outstanding holds are read. A hold changes over minutes to
/// days, so one read per typical scrape interval is enough.
const COLLECTION_INTERVAL: Duration = Duration::from_secs(60);

/// Deadline for one read, from acquiring its connection to its last statement.
/// Shorter than [`COLLECTION_INTERVAL`], so a slow database costs that read
/// without delaying the next.
const COLLECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Server-side cap on each statement of a read, so one abandoned at the
/// deadline does not keep running. A shorter site-wide limit still applies.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a read stays published. Long enough to bridge one failed read,
/// including one that ran to its deadline; after a second, the series stop
/// instead of repeating a count nobody has confirmed.
const HOLD_PERIOD: Duration = COLLECTION_INTERVAL
    .saturating_mul(2)
    .saturating_add(COLLECTION_TIMEOUT);

/// Hosts per snapshot query when checking for an SLA waiver. Bounds the
/// snapshots held in memory at once; only held hosts already past the limit of
/// their state are loaded at all.
const SNAPSHOT_BATCH_SIZE: usize = 500;

/// Most hosts whose health one read loads to check for an SLA waiver. The
/// lookup measured about 0.45 ms per host on a 100,000-host test database, so
/// this many cost about half a second there and still fit the deadline on a
/// database several times slower. Held hosts past their limit beyond this
/// number are counted as above SLA unchecked. A waiver can only take a host
/// out of that count, so the cap can overstate it but never hides a host, and
/// with more than a thousand such hosts one waiver changes nothing an operator
/// does.
const MAX_HEALTH_LOOKUPS_PER_READ: usize = 1_000;

/// Hosts and the longest wait for one label combination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HoldSeries {
    hosts: u64,
    oldest_age_seconds: u64,
}

/// One read of the outstanding holds, summarized per label combination.
#[derive(Debug, Default, PartialEq, Eq)]
struct HoldMetrics {
    /// Indexed by `above_sla`, then by `in_use`.
    series: [[HoldSeries; 2]; 2],
}

impl HoldMetrics {
    fn record(
        &mut self,
        above_sla: bool,
        in_use: bool,
        requested_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) {
        let series = &mut self.series[usize::from(above_sla)][usize::from(in_use)];
        series.hosts += 1;
        // A request stamped after `now` is clock skew between the database and
        // this process, not a negative wait.
        let age_seconds = now
            .signed_duration_since(requested_at)
            .to_std()
            .unwrap_or_default()
            .as_secs();
        series.oldest_age_seconds = series.oldest_age_seconds.max(age_seconds);
    }

    /// Every label combination with its series. Empty ones are included, so a
    /// combination reads zero rather than disappearing when its last hold is
    /// released.
    fn labelled(&self) -> impl Iterator<Item = ([KeyValue; 2], HoldSeries)> + '_ {
        [false, true].into_iter().flat_map(move |above_sla| {
            [false, true].into_iter().map(move |in_use| {
                (
                    [
                        KeyValue::new("above_sla", above_sla.to_string()),
                        KeyValue::new("in_use", in_use.to_string()),
                    ],
                    self.series[usize::from(above_sla)][usize::from(in_use)],
                )
            })
        })
    }
}

/// Which of the held hosts that are past the limit of their state get their
/// health loaded in one read.
struct HealthLookupPlan {
    /// At most the cap, longest wait first.
    lookups: Vec<PendingActionHost>,
    /// The rest, counted as above SLA without a lookup.
    unchecked: Vec<PendingActionHost>,
}

impl HealthLookupPlan {
    fn new(mut past_limit: Vec<PendingActionHost>, max_lookups: usize) -> Self {
        // Longest wait first, then machine ID, so that every replica and every
        // read give the lookups to the same hosts for as long as the held
        // hosts stay the same.
        past_limit.sort_unstable_by_key(|hold| (hold.requested_at, hold.host_id));
        let unchecked = past_limit.split_off(max_lookups.min(past_limit.len()));
        Self {
            lookups: past_limit,
            unchecked,
        }
    }

    /// Counts the unchecked hosts as above SLA, each under its own tenancy.
    fn record_unchecked(&self, metrics: &mut HoldMetrics, now: DateTime<Utc>) {
        for hold in &self.unchecked {
            metrics.record(true, hold.has_instance, hold.requested_at, now);
        }
    }

    /// Logs once per read while the cap leaves hosts unchecked.
    fn warn_if_capped(&self) {
        if self.unchecked.is_empty() {
            return;
        }
        tracing::warn!(
            checked_host_count = self.lookups.len(),
            unchecked_host_count = self.unchecked.len(),
            "Too many held hosts are past their state SLA to check each for an SLA waiver; the unchecked hosts are counted as above SLA"
        );
    }
}

fn register_gauges(meter: &Meter, metrics: SharedMetricsHolder<HoldMetrics>) {
    {
        let metrics = metrics.clone();
        meter
            .u64_observable_gauge("carbide_hosts_dpu_service_sync_pending_count")
            .with_description(
                "Number of hosts whose DPUService rollout hold is still outstanding, by whether the host is past the SLA of its current state (above_sla) and whether an instance is allocated on it (in_use)",
            )
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    for (labels, series) in metrics.labelled() {
                        observer.observe(series.hosts, &[attrs, labels.as_slice()].concat());
                    }
                })
            })
            .build();
    }
    meter
        .u64_observable_gauge("carbide_hosts_dpu_service_sync_oldest_pending_age_seconds")
        .with_description(
            "Age in seconds of the oldest outstanding DPUService rollout hold, measured from when the hold was first requested, by whether the host is past the SLA of its current state (above_sla) and whether an instance is allocated on it (in_use); 0 when no hold is outstanding",
        )
        .with_callback(move |observer| {
            metrics.if_available(|metrics, attrs| {
                for (labels, series) in metrics.labelled() {
                    observer.observe(
                        series.oldest_age_seconds,
                        &[attrs, labels.as_slice()].concat(),
                    );
                }
            })
        })
        .build();
}

/// Why a read of the outstanding holds produced nothing to publish.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HoldReadError {
    #[error("reading the outstanding holds exceeded its {}s deadline", COLLECTION_TIMEOUT.as_secs())]
    DeadlineExceeded,
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// Reads the outstanding DPU service sync holds on a timer and publishes them
/// as gauges.
pub(crate) struct DpuServiceSyncHoldMonitor {
    database_connection: sqlx::PgPool,
    host_health: HostHealthConfig,
    sla_config: MachineSlaConfig,
    /// [`MAX_HEALTH_LOOKUPS_PER_READ`] outside tests.
    max_health_lookups: usize,
    metrics: SharedMetricsHolder<HoldMetrics>,
}

impl DpuServiceSyncHoldMonitor {
    /// `host_health` and `sla_config` must be the machine state controller's,
    /// so that "past SLA" here is its answer for the same host.
    pub(crate) fn new(
        database_connection: sqlx::PgPool,
        meter: &Meter,
        host_health: HostHealthConfig,
        sla_config: MachineSlaConfig,
    ) -> Self {
        let metrics = SharedMetricsHolder::with_hold_period(HOLD_PERIOD);
        register_gauges(meter, metrics.clone());
        Self {
            database_connection,
            host_health,
            sla_config,
            max_health_lookups: MAX_HEALTH_LOOKUPS_PER_READ,
            metrics,
        }
    }

    /// Lowers the lookup cap so that a test can reach it with a few hosts.
    #[cfg(test)]
    pub(crate) fn set_max_health_lookups(&mut self, max_health_lookups: usize) {
        self.max_health_lookups = max_health_lookups;
    }

    pub(crate) fn start(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> io::Result<()> {
        join_set
            .build_task()
            .name("dpu_service_sync_hold_monitor")
            .spawn(async move { self.run(cancel_token).await })?;
        Ok(())
    }

    async fn run(&self, cancel_token: CancellationToken) {
        let timer = PeriodicTimer::new(COLLECTION_INTERVAL);
        loop {
            let tick = timer.tick();
            // A read abandoned at shutdown publishes nothing and discards its
            // connection, the same as one that reaches its deadline.
            let Some(read) = cancel_token
                .run_until_cancelled(self.run_single_iteration())
                .await
            else {
                break;
            };
            if let Err(error) = read {
                tracing::warn!(
                    %error,
                    "Could not read the outstanding DPU service sync holds; their gauges keep the previous read until it expires"
                );
            }
            if cancel_token
                .run_until_cancelled(tick.sleep())
                .await
                .is_none()
            {
                break;
            }
        }
        tracing::info!("DpuServiceSyncHoldMonitor stop was requested");
    }

    /// Reads the holds once and publishes the result.
    ///
    /// A failed read publishes nothing: the gauges keep the previous read until
    /// it expires and then stop, so a database that could not be read is never
    /// reported as zero holds.
    pub(crate) async fn run_single_iteration(&self) -> Result<(), HoldReadError> {
        let metrics = tokio::time::timeout(COLLECTION_TIMEOUT, self.read_holds())
            .await
            .map_err(|_| HoldReadError::DeadlineExceeded)??;
        self.metrics.update(metrics);
        Ok(())
    }

    async fn read_holds(&self) -> Result<HoldMetrics, DatabaseError> {
        // Dropping the guard at the deadline discards the connection instead of
        // waiting for it to be cleaned up.
        let mut session = DbSessionGuard::acquire(&self.database_connection).await?;
        let read = self.read_snapshot(&mut session).await;
        // The read ran to its end, with or without an error, so its transaction
        // is committed or rolled back before the connection is used again. Only
        // a read that is abandoned discards its connection.
        session.return_to_pool();
        read
    }

    async fn read_snapshot(
        &self,
        session: &mut DbSessionGuard,
    ) -> Result<HoldMetrics, DatabaseError> {
        let now = Utc::now();
        // One snapshot for both statements, so a host cannot change state or
        // disappear between them.
        let mut txn = session.begin_read_only_snapshot(STATEMENT_TIMEOUT).await?;
        let holds =
            db::machine_pending_action::find_outstanding_hosts(&mut txn, DpuServiceSync).await?;

        // Whether a host is past its SLA also depends on its aggregate health,
        // because an `ExcludeFromStateMachineSla` alert waives the SLA. Health
        // costs a snapshot per host, and during a rollout most held hosts are
        // tenants' hosts sitting in a state that has no SLA at all. Health can
        // only waive an SLA, never impose one, so a host that is within its
        // limit without any alerts needs no snapshot. Only the rest are loaded,
        // up to a cap, to give a waiver its say.
        let no_alerts = HealthReport::empty(String::new());
        let (past_limit, within_limit): (Vec<_>, Vec<_>) = holds
            .into_iter()
            .partition(|hold| self.is_above_sla(hold, &no_alerts));

        let mut metrics = HoldMetrics::default();
        for hold in &within_limit {
            metrics.record(false, hold.has_instance, hold.requested_at, now);
        }
        let plan = HealthLookupPlan::new(past_limit, self.max_health_lookups);
        plan.record_unchecked(&mut metrics, now);
        for batch in plan.lookups.chunks(SNAPSHOT_BATCH_SIZE) {
            let host_ids: Vec<_> = batch.iter().map(|hold| hold.host_id).collect();
            let snapshots =
                db::managed_host::load_compact_by_host_ids(&mut txn, &host_ids, self.host_health)
                    .await?;
            for hold in batch {
                // Without a snapshot there is no health to waive the SLA the
                // host's state has already exceeded.
                let above_sla = snapshots
                    .get(&hold.host_id)
                    .is_none_or(|snapshot| self.is_above_sla(hold, &snapshot.aggregate_health));
                metrics.record(above_sla, hold.has_instance, hold.requested_at, now);
            }
        }

        txn.commit().await?;
        // Warned only after every lookup succeeded, so that the counts describe
        // checks that were made.
        plan.warn_if_capped();
        Ok(metrics)
    }

    /// The state controller's own SLA verdict for the host's current state.
    fn is_above_sla(&self, hold: &PendingActionHost, aggregate_health: &HealthReport) -> bool {
        state_sla(
            &hold.host_id,
            &hold.state.value,
            &hold.state.version,
            aggregate_health,
            &self.sla_config,
        )
        .time_in_state_above_sla
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_instrument::testing::capture_logs;
    use carbide_test_support::{Check, check_values};
    use carbide_utils::test_support::test_meter::TestMeter;
    use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
    use config_version::{ConfigVersion, Versioned};
    use model::machine::ManagedHostState;

    use super::*;

    const PENDING_COUNT: &str = "carbide_hosts_dpu_service_sync_pending_count";
    const OLDEST_AGE: &str = "carbide_hosts_dpu_service_sync_oldest_pending_age_seconds";

    /// A hold as the summary sees it: its labels and how long ago it was
    /// requested, negative for a request stamped after the read.
    struct Hold {
        above_sla: bool,
        in_use: bool,
        requested_seconds_ago: i64,
    }

    fn hold(above_sla: bool, in_use: bool, requested_seconds_ago: i64) -> Hold {
        Hold {
            above_sla,
            in_use,
            requested_seconds_ago,
        }
    }

    fn series(hosts: u64, oldest_age_seconds: u64) -> HoldSeries {
        HoldSeries {
            hosts,
            oldest_age_seconds,
        }
    }

    #[test]
    fn holds_are_summarized_per_label_combination() {
        // `expect` is indexed like `HoldMetrics::series`: by `above_sla`, then
        // by `in_use`.
        check_values(
            [
                Check {
                    scenario: "no holds",
                    input: vec![],
                    expect: [[series(0, 0), series(0, 0)], [series(0, 0), series(0, 0)]],
                },
                Check {
                    scenario: "each label combination is kept apart",
                    input: vec![
                        hold(false, false, 10),
                        hold(false, true, 20),
                        hold(true, false, 30),
                        hold(true, true, 40),
                    ],
                    expect: [
                        [series(1, 10), series(1, 20)],
                        [series(1, 30), series(1, 40)],
                    ],
                },
                Check {
                    scenario: "the longest wait in a combination is its age",
                    input: vec![
                        hold(true, false, 60),
                        hold(true, false, 7200),
                        hold(true, false, 300),
                    ],
                    expect: [
                        [series(0, 0), series(0, 0)],
                        [series(3, 7200), series(0, 0)],
                    ],
                },
                Check {
                    scenario: "a request stamped after the read is counted with no age",
                    input: vec![hold(false, true, -5)],
                    expect: [[series(0, 0), series(1, 0)], [series(0, 0), series(0, 0)]],
                },
            ],
            |holds| {
                let now = Utc::now();
                let mut metrics = HoldMetrics::default();
                for hold in holds {
                    metrics.record(
                        hold.above_sla,
                        hold.in_use,
                        now - chrono::Duration::seconds(hold.requested_seconds_ago),
                        now,
                    );
                }
                metrics.series
            },
        );
    }

    /// The exported samples of both gauges, sorted, without HELP and TYPE.
    fn exported_samples(meter: &TestMeter) -> Vec<String> {
        let mut samples: Vec<_> = meter
            .export_metrics()
            .lines()
            .filter(|line| line.starts_with(PENDING_COUNT) || line.starts_with(OLDEST_AGE))
            .map(str::to_string)
            .collect();
        samples.sort_unstable();
        samples
    }

    #[test]
    fn gauges_follow_the_latest_read_and_stop_without_one() {
        let read = || {
            let now = Utc::now();
            let mut metrics = HoldMetrics::default();
            metrics.record(true, false, now - chrono::Duration::seconds(7200), now);
            metrics.record(false, true, now - chrono::Duration::seconds(90), now);
            metrics.record(false, true, now - chrono::Duration::seconds(30), now);
            metrics
        };

        check_values(
            [
                Check {
                    scenario: "nothing is exported before the first read",
                    input: (Duration::MAX, false),
                    expect: vec![],
                },
                Check {
                    scenario: "a read exports every label combination, empty ones as zero",
                    input: (Duration::MAX, true),
                    expect: vec![
                        format!(r#"{OLDEST_AGE}{{above_sla="false",in_use="false"}} 0"#),
                        format!(r#"{OLDEST_AGE}{{above_sla="false",in_use="true"}} 90"#),
                        format!(r#"{OLDEST_AGE}{{above_sla="true",in_use="false"}} 7200"#),
                        format!(r#"{OLDEST_AGE}{{above_sla="true",in_use="true"}} 0"#),
                        format!(r#"{PENDING_COUNT}{{above_sla="false",in_use="false"}} 0"#),
                        format!(r#"{PENDING_COUNT}{{above_sla="false",in_use="true"}} 2"#),
                        format!(r#"{PENDING_COUNT}{{above_sla="true",in_use="false"}} 1"#),
                        format!(r#"{PENDING_COUNT}{{above_sla="true",in_use="true"}} 0"#),
                    ],
                },
                Check {
                    scenario: "an expired read is not exported as a stale count or as zero",
                    input: (Duration::ZERO, true),
                    expect: vec![],
                },
            ],
            |(hold_period, publish)| {
                let meter = TestMeter::default();
                let metrics = SharedMetricsHolder::with_hold_period(hold_period);
                register_gauges(&meter.meter(), metrics.clone());
                if publish {
                    metrics.update(read());
                }
                exported_samples(&meter)
            },
        );
    }

    /// A held host as the lookup plan sees it: not in use, with a machine ID
    /// that `marker` makes distinct. The plan never reads the state.
    fn held_host(marker: u8, requested_at: DateTime<Utc>) -> PendingActionHost {
        let mut hardware_id = [0u8; 32];
        hardware_id[0] = marker;
        PendingActionHost {
            host_id: MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                hardware_id,
                MachineType::Host,
            )
            .try_into()
            .expect("a host machine ID"),
            requested_at,
            state: Versioned::new(ManagedHostState::Ready, ConfigVersion::initial()),
            has_instance: false,
        }
    }

    #[test]
    fn health_lookups_go_to_the_longest_waits_up_to_the_cap() {
        let now = Utc::now();
        // Two hosts that tie on request time. Which sorts first is the machine
        // ID order, read from the IDs so the test does not assume it.
        let (lower, higher) = if held_host(1, now).host_id < held_host(2, now).host_id {
            (1, 2)
        } else {
            (2, 1)
        };

        // Input: `(marker, seconds since the request, in use)` per host, and
        // the cap. Expect: the markers that get a lookup, the unchecked ones,
        // and how many hosts the unchecked ones add to above SLA, first not in
        // use and then in use.
        check_values(
            [
                Check {
                    scenario: "under the cap every host is looked up, longest wait first",
                    input: (vec![(1, 10, false), (2, 30, true), (3, 20, false)], 5),
                    expect: (vec![2, 3, 1], vec![], [0, 0]),
                },
                Check {
                    scenario: "exactly at the cap no host is left unchecked",
                    input: (vec![(1, 10, false), (2, 30, true), (3, 20, false)], 3),
                    expect: (vec![2, 3, 1], vec![], [0, 0]),
                },
                Check {
                    scenario: "one over the cap leaves the newest hold unchecked",
                    input: (vec![(1, 10, false), (2, 30, true), (3, 20, false)], 2),
                    expect: (vec![2, 3], vec![1], [1, 0]),
                },
                Check {
                    scenario: "equal waits are ordered by machine ID, and each unchecked host keeps its own tenancy",
                    input: (
                        vec![(higher, 100, true), (lower, 100, false), (3, 50, false)],
                        1,
                    ),
                    expect: (vec![lower], vec![higher, 3], [1, 1]),
                },
            ],
            |(holds, cap): (Vec<(u8, i64, bool)>, usize)| {
                let hosts: Vec<_> = holds
                    .iter()
                    .map(|&(marker, seconds_ago, in_use)| PendingActionHost {
                        has_instance: in_use,
                        ..held_host(marker, now - chrono::Duration::seconds(seconds_ago))
                    })
                    .collect();
                let marker_by_id: HashMap<_, _> = hosts
                    .iter()
                    .zip(&holds)
                    .map(|(host, &(marker, _, _))| (host.host_id, marker))
                    .collect();
                // A plan as markers, with what its unchecked hosts record.
                let outcome = |plan: HealthLookupPlan| {
                    let markers = |hosts: &[PendingActionHost]| {
                        hosts
                            .iter()
                            .map(|host| marker_by_id[&host.host_id])
                            .collect::<Vec<_>>()
                    };
                    let mut metrics = HoldMetrics::default();
                    plan.record_unchecked(&mut metrics, now);
                    assert_eq!(
                        metrics.series[0],
                        [HoldSeries::default(); 2],
                        "an unchecked host is never counted as within its SLA"
                    );
                    (
                        markers(&plan.lookups),
                        markers(&plan.unchecked),
                        [metrics.series[1][0].hosts, metrics.series[1][1].hosts],
                    )
                };

                let planned = outcome(HealthLookupPlan::new(hosts.clone(), cap));
                let reversed = outcome(HealthLookupPlan::new(
                    hosts.into_iter().rev().collect(),
                    cap,
                ));
                assert_eq!(
                    planned, reversed,
                    "the plan must not depend on the order the holds were read in"
                );
                planned
            },
        );
    }

    #[test]
    fn a_read_warns_only_while_the_cap_leaves_hosts_unchecked() {
        // Input: the cap, for three held hosts past their limit.
        // Expect: each log as (level, message, checked count, unchecked count).
        check_values(
            [
                Check {
                    scenario: "every host fits under the cap",
                    input: 3,
                    expect: vec![],
                },
                Check {
                    scenario: "the cap leaves one host unchecked",
                    input: 2,
                    expect: vec![(
                        tracing::Level::WARN,
                        "Too many held hosts are past their state SLA to check each for an SLA waiver; the unchecked hosts are counted as above SLA".to_string(),
                        Some("2".to_string()),
                        Some("1".to_string()),
                    )],
                },
            ],
            |cap| {
                let now = Utc::now();
                let plan = HealthLookupPlan::new(
                    (1..=3).map(|marker| held_host(marker, now)).collect(),
                    cap,
                );
                capture_logs(|| plan.warn_if_capped())
                    .into_iter()
                    .map(|log| {
                        (
                            log.level,
                            log.message.clone(),
                            log.field("checked_host_count").map(str::to_string),
                            log.field("unchecked_host_count").map(str::to_string),
                        )
                    })
                    .collect::<Vec<_>>()
            },
        );
    }
}
