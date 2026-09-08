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

mod metrics;

use std::default::Default;
use std::io;
use std::sync::Arc;

use carbide_machine_controller::config::machine_validation::MachineValidationConfig;
use carbide_utils::managed_loop::{self, LoopManager};
use carbide_utils::periodic_timer::PeriodicTimer;
use db::ObjectColumnFilter;
use db::machine_validation::StateColumn;
use model::machine::{FailureCause, FailureDetails, FailureSource};
use model::machine_validation::{
    MachineValidation, MachineValidationRunItem, MachineValidationRunItemState,
    MachineValidationState, MachineValidationStatus,
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use self::metrics::MachineValidationMetrics;
use crate::CarbideResult;

/// The terminal outcome of a machine validation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
pub(crate) enum MachineValidationOutcome {
    Passed,
    Failed,
}

/// Why a machine validation run failed, in the vocabulary of the
/// health-report alert ids the completion paths record. `None` is the label
/// value for a passed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
pub(crate) enum MachineValidationFailureCause {
    None,
    FailedValidationRunItems,
    FailedValidationTest,
    FailedValidationTestCompletion,
    StaleMachineValidationAttempt,
    StaleMachineValidationRun,
}

impl MachineValidationFailureCause {
    /// The health-report alert id recorded for this failure cause; `None` is
    /// the passed-run label value and records no alert.
    pub(crate) fn health_alert_id(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::FailedValidationRunItems => Some("FailedValidationRunItems"),
            Self::FailedValidationTest => Some("FailedValidationTest"),
            Self::FailedValidationTestCompletion => Some("FailedValidationTestCompletion"),
            Self::StaleMachineValidationAttempt => Some("StaleMachineValidationAttempt"),
            Self::StaleMachineValidationRun => Some("StaleMachineValidationRun"),
        }
    }
}

/// A machine validation run completed as passed or failed -- reported by
/// scout through the completion handler, or reconciled by the manager when
/// every run item is terminal or the run goes stale. Every emitting path
/// sits behind the run's single active-to-terminal transition in the
/// database, so a run counts at most once and the per-cause rate is the
/// validation pass/fail funnel.
///
/// Runs that a disabled validation config skips are deliberately not
/// counted: the machine controller flips them to `Skipped` through the same
/// database gate without emitting. If skips ever become worth counting, an
/// `outcome = Skipped` variant slots in beside `Passed`/`Failed`.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "machine_validation_completed",
    metric_name = "carbide_machine_validation_outcomes_total",
    component = "nico-api",
    log = dynamic,
    metric = counter,
    message = "machine validation completed",
    describe = "Number of machine validation runs that completed as passed or failed, by outcome and failure cause; runs skipped by a disabled validation config are not counted"
)]
pub(crate) struct MachineValidationCompleted {
    #[label]
    pub(crate) outcome: MachineValidationOutcome,
    #[label]
    pub(crate) cause: MachineValidationFailureCause,
    #[context]
    pub(crate) machine_id: carbide_uuid::machine::MachineId,
    #[context]
    pub(crate) validation_id: carbide_uuid::machine_validation::MachineValidationId,
    #[context]
    pub(crate) error: String,
}

/// A passed run logs as routine progress; a failed run logs at the warning
/// level the stale-run reconciler already used.
impl carbide_instrument::DynamicLog for MachineValidationCompleted {
    fn log_at(&self) -> carbide_instrument::LogAt {
        match self.outcome {
            MachineValidationOutcome::Passed => {
                carbide_instrument::LogAt::Level(tracing::Level::INFO)
            }
            MachineValidationOutcome::Failed => {
                carbide_instrument::LogAt::Level(tracing::Level::WARN)
            }
        }
    }
}

pub(crate) struct MachineValidationManager {
    database_connection: sqlx::PgPool,
    config: MachineValidationConfig,
    metric_holder: Arc<metrics::MetricHolder>,
}

impl MachineValidationManager {
    pub(crate) fn new(
        database_connection: sqlx::PgPool,
        mut config: MachineValidationConfig,
        meter: opentelemetry::metrics::Meter,
    ) -> Self {
        if config.stale_run_timeout < MachineValidationConfig::MIN_STALE_RUN_TIMEOUT {
            tracing::warn!(
                configured_stale_run_timeout_seconds = config.stale_run_timeout.as_secs(),
                minimum_stale_run_timeout_seconds =
                    MachineValidationConfig::MIN_STALE_RUN_TIMEOUT.as_secs(),
                "machine validation stale_run_timeout is below minimum; using minimum"
            );
            config.stale_run_timeout = MachineValidationConfig::MIN_STALE_RUN_TIMEOUT;
        }

        let hold_period = config
            .run_interval
            .saturating_add(std::time::Duration::from_secs(60));

        let metric_holder = Arc::new(metrics::MetricHolder::new(meter, hold_period));

        MachineValidationManager {
            database_connection,
            config,
            metric_holder,
        }
    }
    pub(crate) fn start(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> io::Result<()> {
        if self.config.enabled {
            join_set
                .build_task()
                .name("machine_validation_manager")
                .spawn(async move { self.run(cancel_token).await })?;
        }
        Ok(())
    }

    async fn run(&self, cancel_token: CancellationToken) {
        let timer = PeriodicTimer::new(self.config.run_interval);
        loop {
            let tick = timer.tick();
            let result = self.run_single_iteration().await;
            managed_loop::record_iteration(LoopManager::MachineValidationManager, &result);

            tokio::select! {
                _ = tick.sleep() => {},
                _ = cancel_token.cancelled() => {
                    tracing::info!("MachineValidationManager stop was requested");
                    return;
                }
            }
        }
    }

    /// run_single_iteration runs a single iteration of the state machine across all explored endpoints in the preingestion state.
    /// Returns true if we stopped early due to a timeout.
    pub(crate) async fn run_single_iteration(&self) -> CarbideResult<()> {
        let mut metrics = MachineValidationMetrics::new();
        let now = chrono::Utc::now();
        let heartbeat_stale_timeout = heartbeat_stale_timeout(self.config.stale_run_timeout);

        // Each reconciliation phase gets its own transaction. PostgreSQL
        // keeps row locks until commit, so sharing a transaction would let a
        // later phase restart from a lower MachineId while higher-ID locks
        // from an earlier phase were still held, recreating SQLSTATE 40P01
        // (`deadlock_detected`) with another ordered multi-machine writer.
        let mut txn = db::Transaction::begin(&self.database_connection).await?;
        let mut completions = Vec::new();
        for validation in db::machine::MachineRowLockOrderIter::new(
            db::machine_validation::find_active(&mut txn).await?,
        ) {
            if let Some(completion) =
                reconcile_terminal_run_items(txn.as_pgconn(), validation).await?
            {
                completions.push(completion);
            }
        }
        txn.commit().await?;
        // Emit only after this phase is durable. Otherwise a rollback would
        // leave the validation active and a later retry would count it again.
        completions.into_iter().for_each(carbide_instrument::emit);

        let mut txn = db::Transaction::begin(&self.database_connection).await?;
        let mut completions = Vec::new();
        for stale_attempt in db::machine::MachineRowLockOrderIter::new(
            db::machine_validation_execution::find_stale_active_attempts(
                &mut txn,
                heartbeat_stale_timeout,
                now,
            )
            .await?,
        )
        .filter(|attempt| attempt.last_heartbeat_at.is_some())
        {
            if let Some(completion) =
                reconcile_stale_attempt(txn.as_pgconn(), stale_attempt, now).await?
            {
                metrics.stale_validation += 1;
                completions.push(completion);
            }
        }
        txn.commit().await?;
        completions.into_iter().for_each(carbide_instrument::emit);

        let mut txn = db::Transaction::begin(&self.database_connection).await?;
        let mut completions = Vec::new();
        for validation in db::machine::MachineRowLockOrderIter::new(stale_validations(
            db::machine_validation::find_active(&mut txn).await?,
            self.config.stale_run_timeout,
            heartbeat_stale_timeout,
            now,
        )) {
            if let Some(completion) = reconcile_stale_validation(
                txn.as_pgconn(),
                validation,
                self.config.stale_run_timeout,
                now,
            )
            .await?
            {
                metrics.stale_validation += 1;
                completions.push(completion);
            }
        }

        metrics.completed_validation = db::machine_validation::find_by(
            &mut txn,
            ObjectColumnFilter::List(StateColumn, &["Success".to_string()]),
        )
        .await?
        .len();

        metrics.failed_validation = db::machine_validation::find_by(
            &mut txn,
            ObjectColumnFilter::List(StateColumn, &["Failed".to_string()]),
        )
        .await?
        .len();
        metrics.in_progress_validation = db::machine_validation::find_by(
            &mut txn,
            ObjectColumnFilter::List(StateColumn, &["InProgress".to_string()]),
        )
        .await?
        .len();

        metrics.oldest_active_validation_age_seconds =
            db::machine_validation::find_active(&mut txn)
                .await?
                .iter()
                .filter_map(|validation| active_validation_age_seconds(validation, now))
                .max()
                .unwrap_or_default();

        metrics.tests = db::machine_validation_suites::find(
            &mut txn,
            model::machine_validation::MachineValidationTestsGetRequest::default(),
        )
        .await?;
        tracing::debug!(
            completed_validation_count = metrics.completed_validation,
            failed_validation_count = metrics.failed_validation,
            in_progress_validation_count = metrics.in_progress_validation,
            "Machine validation metrics computed",
        );
        self.metric_holder.update_metrics(metrics);

        txn.commit().await?;
        completions.into_iter().for_each(carbide_instrument::emit);

        Ok(())
    }
}

fn active_validation_age_seconds(
    validation: &MachineValidation,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<u64> {
    validation
        .start_time
        .and_then(|start_time| now.signed_duration_since(start_time).to_std().ok())
        .map(|age| age.as_secs())
}

fn heartbeat_stale_timeout(configured_timeout: std::time::Duration) -> std::time::Duration {
    configured_timeout.max(MachineValidationConfig::MIN_STALE_RUN_TIMEOUT)
}

fn stale_validations(
    validations: Vec<MachineValidation>,
    stale_run_timeout: std::time::Duration,
    heartbeat_stale_timeout: std::time::Duration,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<MachineValidation> {
    validations
        .into_iter()
        .filter(|validation| {
            let stale_run_timeout = chrono::Duration::from_std(stale_run_timeout).ok();
            let heartbeat_stale_timeout = chrono::Duration::from_std(heartbeat_stale_timeout).ok();
            if let (Some(last_heartbeat_at), Some(stale_run_timeout)) =
                (validation.last_heartbeat_at, heartbeat_stale_timeout)
            {
                return last_heartbeat_at + stale_run_timeout < now;
            }

            validation
                .start_time
                .and_then(|start_time| {
                    let expected_duration =
                        chrono::Duration::seconds(validation.duration_to_complete.max(0));
                    let stale_run_timeout = stale_run_timeout?;
                    Some(start_time + expected_duration + stale_run_timeout)
                })
                .is_some_and(|stale_after| stale_after < now)
        })
        .collect()
}

// Each reconcile function returns the completion event for the run it
// transitioned (`None` when another path already completed it) instead of
// emitting: the caller's transaction is still open, and the event must not
// count a transition that later rolls back.
async fn reconcile_terminal_run_items(
    txn: &mut sqlx::PgConnection,
    validation: MachineValidation,
) -> CarbideResult<Option<MachineValidationCompleted>> {
    let run_items =
        db::machine_validation_execution::find_run_items_by_run_id(&mut *txn, &validation.id)
            .await?;

    if run_items.is_empty() || !run_items.iter().all(run_item_is_terminal) {
        return Ok(None);
    }

    if run_items
        .iter()
        .any(|item| item.state == MachineValidationRunItemState::Failed)
    {
        let failed_items = run_items
            .iter()
            .filter(|item| item.state == MachineValidationRunItemState::Failed)
            .map(|item| item.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let error_message = format!(
            "Machine validation run {} completed with failed run item(s): {}",
            validation.id, failed_items
        );
        return complete_active_validation_as_failed(
            txn,
            &validation.id,
            error_message,
            MachineValidationFailureCause::FailedValidationRunItems,
        )
        .await;
    }

    let status = MachineValidationStatus {
        state: MachineValidationState::Success,
        ..MachineValidationStatus::default()
    };
    let completed = db::machine_validation::mark_machine_validation_complete(
        txn,
        &validation.machine_id,
        &validation.id,
        status,
    )
    .await?;
    Ok(completed.then(|| MachineValidationCompleted {
        outcome: MachineValidationOutcome::Passed,
        cause: MachineValidationFailureCause::None,
        machine_id: validation.machine_id,
        validation_id: validation.id,
        error: String::new(),
    }))
}

fn run_item_is_terminal(run_item: &MachineValidationRunItem) -> bool {
    matches!(
        run_item.state,
        MachineValidationRunItemState::Success
            | MachineValidationRunItemState::Skipped
            | MachineValidationRunItemState::Failed
    )
}

async fn reconcile_stale_attempt(
    txn: &mut sqlx::PgConnection,
    stale_attempt: db::machine_validation_execution::StaleMachineValidationAttempt,
    now: chrono::DateTime<chrono::Utc>,
) -> CarbideResult<Option<MachineValidationCompleted>> {
    let error_message = format!(
        "Machine validation attempt {} for test {} in run {} stopped heartbeating or exceeded its timeout",
        stale_attempt.attempt_id, stale_attempt.test_id, stale_attempt.validation_id
    );

    // Keep the same parent-run -> run-item lock order used by result
    // persistence and heartbeats.
    if db::machine_validation::lock_by_id_no_key_update(txn, &stale_attempt.validation_id)
        .await?
        .is_none()
    {
        tracing::debug!(
            validation_id = %stale_attempt.validation_id,
            attempt_id = %stale_attempt.attempt_id,
            "skipping stale machine validation attempt because run no longer exists"
        );
        return Ok(None);
    }

    let Some(validation_id) = db::machine_validation_execution::mark_attempt_stale_if_active(
        txn,
        &stale_attempt.attempt_id,
        now,
        &error_message,
    )
    .await?
    else {
        tracing::debug!(
            attempt_id = %stale_attempt.attempt_id,
            "skipping stale machine validation attempt because it is no longer active"
        );
        return Ok(None);
    };

    complete_active_validation_as_failed(
        txn,
        &validation_id,
        error_message,
        MachineValidationFailureCause::StaleMachineValidationAttempt,
    )
    .await
}

async fn complete_active_validation_as_failed(
    txn: &mut sqlx::PgConnection,
    validation_id: &carbide_uuid::machine_validation::MachineValidationId,
    error_message: String,
    cause: MachineValidationFailureCause,
) -> CarbideResult<Option<MachineValidationCompleted>> {
    let validation = db::machine_validation::find_by_id(&mut *txn, validation_id).await?;
    let status = MachineValidationStatus {
        state: MachineValidationState::Failed,
        ..MachineValidationStatus::default()
    };

    let completed = db::machine_validation::mark_machine_validation_complete(
        txn,
        &validation.machine_id,
        &validation.id,
        status,
    )
    .await?;

    if !completed {
        return Ok(None);
    }

    let completion =
        record_failed_validation_side_effects(txn, &validation, error_message, cause).await?;
    Ok(Some(completion))
}

async fn reconcile_stale_validation(
    txn: &mut sqlx::PgConnection,
    validation: MachineValidation,
    stale_run_timeout: std::time::Duration,
    now: chrono::DateTime<chrono::Utc>,
) -> CarbideResult<Option<MachineValidationCompleted>> {
    // Returns the completion only when this call actually transitions an
    // active stale run. `None` means another path already completed or
    // reconciled the run.
    let error_message = format!(
        "Machine validation run {} exceeded its expected duration plus stale timeout",
        validation.id
    );

    let status = MachineValidationStatus {
        state: MachineValidationState::Failed,
        ..MachineValidationStatus::default()
    };

    let Some(validation) = db::machine_validation::mark_stale_if_active(
        txn,
        &validation.id,
        stale_run_timeout,
        now,
        &status,
    )
    .await?
    else {
        tracing::debug!(
            machine_validation_id = %validation.id,
            "skipping stale machine validation because it is no longer active or stale"
        );
        return Ok(None);
    };

    let completion = record_failed_validation_side_effects(
        txn,
        &validation,
        error_message,
        MachineValidationFailureCause::StaleMachineValidationRun,
    )
    .await?;

    Ok(Some(completion))
}

async fn record_failed_validation_side_effects(
    txn: &mut sqlx::PgConnection,
    validation: &MachineValidation,
    error_message: String,
    cause: MachineValidationFailureCause,
) -> CarbideResult<MachineValidationCompleted> {
    // The caller just transitioned this run from active to terminal, so this
    // builds the run's one completion event; the manager emits it after the
    // iteration's transaction commits. The run counts even when the owning
    // machine lookup below comes up empty.
    let completion = MachineValidationCompleted {
        outcome: MachineValidationOutcome::Failed,
        cause,
        machine_id: validation.machine_id,
        validation_id: validation.id,
        error: error_message.clone(),
    };

    let Some(alert_id) = cause.health_alert_id() else {
        // Unreachable from the completion paths: every caller passes a
        // failure cause. Without an alert id there are no side effects to
        // record.
        return Ok(completion);
    };

    let Some(machine) = db::machine::find_by_validation_id(txn, &validation.id).await? else {
        tracing::warn!(
            machine_validation_id = %validation.id,
            machine_id = %validation.machine_id,
            "failed machine validation has no owning machine"
        );
        return Ok(completion);
    };

    db::machine::update_failure_details_by_machine_id(
        &machine.id,
        txn,
        FailureDetails {
            cause: FailureCause::MachineValidation {
                err: error_message.clone(),
            },
            failed_at: chrono::Utc::now(),
            source: FailureSource::Scout,
        },
    )
    .await?;

    let mut health_report = machine.machine_validation_health_report();
    health_report.observed_at = Some(chrono::Utc::now());
    health_report.alerts.push(health_report::HealthProbeAlert {
        id: alert_id.parse().unwrap(),
        target: None,
        in_alert_since: Some(chrono::Utc::now()),
        message: error_message.clone(),
        tenant_message: None,
        classifications: vec![health_report::HealthAlertClassification::prevent_allocations()],
    });
    db::machine::update_machine_validation_health_report(txn, &machine.id, &health_report).await?;

    db::machine::set_machine_validation_request(txn, &machine.id, false).await?;
    db::machine::update_machine_validation_time(&machine.id, txn).await?;

    Ok(completion)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::machine::MachineId;
    use carbide_uuid::machine_validation::MachineValidationId;

    use super::*;

    fn validation_started_at(
        start_time: chrono::DateTime<chrono::Utc>,
        duration_to_complete: i64,
    ) -> MachineValidation {
        MachineValidation {
            id: MachineValidationId::new(),
            machine_id: MachineId::from_str(
                "fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30",
            )
            .unwrap(),
            name: "test".to_string(),
            start_time: Some(start_time),
            end_time: None,
            filter: None,
            context: Some("OnDemand".to_string()),
            status: None,
            duration_to_complete,
            last_heartbeat_at: None,
        }
    }

    #[test]
    fn stale_validations_respects_expected_duration_plus_grace() {
        let now = chrono::Utc::now();
        let stale = validation_started_at(now - chrono::Duration::seconds(11), 5);
        let active = validation_started_at(now - chrono::Duration::seconds(9), 5);

        let stale = stale_validations(
            vec![stale, active],
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(90),
            now,
        );

        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn stale_validations_clamps_heartbeat_timeout_above_scout_cadence() {
        let now = chrono::Utc::now();
        let mut active = validation_started_at(now - chrono::Duration::seconds(30), 0);
        active.last_heartbeat_at = Some(now - chrono::Duration::seconds(30));

        let stale = stale_validations(
            vec![active],
            std::time::Duration::from_secs(1),
            heartbeat_stale_timeout(std::time::Duration::from_secs(1)),
            now,
        );

        assert!(stale.is_empty());
    }

    /// One completion emit writes the log line at the outcome's level -- INFO
    /// for a passed run, WARN for every failure cause -- with the ids and
    /// error as fields, AND moves the outcome counter under the snake_case
    /// labels.
    #[test]
    fn completion_logs_and_counts_by_outcome_and_cause() {
        use carbide_instrument::testing::{MetricsCapture, capture_logs};

        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")
                .expect("a valid machine id");
        let validation_id = MachineValidationId::new();

        let combos = [
            (
                MachineValidationOutcome::Passed,
                MachineValidationFailureCause::None,
                "",
            ),
            (
                MachineValidationOutcome::Failed,
                MachineValidationFailureCause::FailedValidationRunItems,
                "run item failed",
            ),
            (
                MachineValidationOutcome::Failed,
                MachineValidationFailureCause::FailedValidationTest,
                "test failed",
            ),
            (
                MachineValidationOutcome::Failed,
                MachineValidationFailureCause::FailedValidationTestCompletion,
                "run did not complete",
            ),
            (
                MachineValidationOutcome::Failed,
                MachineValidationFailureCause::StaleMachineValidationAttempt,
                "attempt stopped heartbeating",
            ),
            (
                MachineValidationOutcome::Failed,
                MachineValidationFailureCause::StaleMachineValidationRun,
                "run exceeded its timeout",
            ),
        ];

        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            for (outcome, cause, error) in combos {
                carbide_instrument::emit(MachineValidationCompleted {
                    outcome,
                    cause,
                    machine_id,
                    validation_id,
                    error: error.to_string(),
                });
            }
        });

        assert_eq!(logs.len(), 6);
        let field = |log: &carbide_instrument::testing::CapturedLog, name: &str| {
            log.fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        for log in &logs {
            assert_eq!(log.message, "machine validation completed");
            assert_eq!(field(log, "machine_id"), Some(machine_id.to_string()));
            assert_eq!(field(log, "validation_id"), Some(validation_id.to_string()));
        }
        assert_eq!(logs[0].level, tracing::Level::INFO);
        assert_eq!(field(&logs[0], "outcome"), Some("passed".to_string()));
        assert_eq!(field(&logs[0], "cause"), Some("none".to_string()));
        assert_eq!(field(&logs[0], "error"), Some(String::new()));
        for (log, cause_label, error) in [
            (&logs[1], "failed_validation_run_items", "run item failed"),
            (&logs[2], "failed_validation_test", "test failed"),
            (
                &logs[3],
                "failed_validation_test_completion",
                "run did not complete",
            ),
            (
                &logs[4],
                "stale_machine_validation_attempt",
                "attempt stopped heartbeating",
            ),
            (
                &logs[5],
                "stale_machine_validation_run",
                "run exceeded its timeout",
            ),
        ] {
            assert_eq!(log.level, tracing::Level::WARN, "cause {cause_label}");
            assert_eq!(field(log, "outcome"), Some("failed".to_string()));
            assert_eq!(field(log, "cause"), Some(cause_label.to_string()));
            assert_eq!(field(log, "error"), Some(error.to_string()));
        }

        // The exact-delta assertion sticks to the one (outcome, cause) pair
        // no DB-gated completion-flow test in this binary drives (the others
        // emit from the product funnels without holding the capture lock), so
        // a parallel run cannot inflate it.
        assert_eq!(
            metrics.counter_delta(
                "carbide_machine_validation_outcomes_total",
                &[
                    ("outcome", "failed"),
                    ("cause", "failed_validation_run_items"),
                ],
            ),
            1.0,
        );
    }
}
