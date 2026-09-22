// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Periodic republishing of current `ManagedHostState`.
//!
//! [`MqttStateChangeHook`](super::hook::MqttStateChangeHook) publishes state on
//! every transition. Integrators that cannot poll the NICo API (e.g. they are
//! network-restricted) can miss a transition and never reconcile. This module
//! walks the current managed hosts on a timer and re-publishes their state to
//! the same `{topic_prefix}/{machineId}/state` topic with the same JSON payload
//! as change-driven events, so downstream consumers handle both identically and
//! can self-heal.
//!
//! Tuning is driven by [`PeriodicStateRepublishConfig`]: sweep `interval`,
//! whether to publish all hosts or only unhealthy ones (`scope`), how often
//! healthy hosts are re-published relative to unhealthy ones
//! (`healthy_republish_every`), and an optional per-sweep publish rate limit
//! (`max_publishes_per_second`).

use std::time::Duration;

use carbide_mqtt_common::hook::{MqttPublisher, publish_with_deadline};
use carbide_mqtt_common::metrics::{MqttHookMetrics, PublishComponent};
use carbide_utils::managed_loop::{self, LoopManager};
use chrono::{DateTime, Utc};
use db::work_lock_manager::{AcquireLockError, WorkLockManagerHandle};
use health_report::HealthReport;
use model::machine::{HostHealthConfig, LoadSnapshotOptions, ManagedHostState};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::cfg::file::{PeriodicStateRepublishConfig, RepublishScope};
use crate::mqtt_state_change_hook::message::ManagedHostStateMessage;

const REPUBLISH_WORK_KEY: &str = "managed_host_state_republisher::iteration";

/// Periodically re-publishes current `ManagedHostState` for managed hosts to
/// the DSX Exchange Event Bus.
///
/// Unlike [`MqttStateChangeHook`](super::hook::MqttStateChangeHook), which
/// buffers change events in a bounded queue, the republisher publishes directly
/// from its sweep. A full sweep of a large site can be many publishes, so it
/// would otherwise risk overflowing (and dropping) the change-event queue;
/// publishing directly keeps the two paths independent and lets
/// `max_publishes_per_second` bound the burst.
pub(crate) struct ManagedHostStateRepublisher<P: MqttPublisher> {
    publisher: P,
    db_pool: sqlx::PgPool,
    work_lock_manager_handle: WorkLockManagerHandle,
    topic_prefix: String,
    publish_timeout: Duration,
    config: PeriodicStateRepublishConfig,
    host_health_config: HostHealthConfig,
    metrics: MqttHookMetrics,
}

/// Constructor parameters for [`ManagedHostStateRepublisher`].
pub(crate) struct ManagedHostStateRepublisherParams {
    pub(crate) db_pool: sqlx::PgPool,
    pub(crate) work_lock_manager_handle: WorkLockManagerHandle,
    pub(crate) topic_prefix: String,
    pub(crate) publish_timeout: Duration,
    pub(crate) config: PeriodicStateRepublishConfig,
    pub(crate) host_health_config: HostHealthConfig,
}

impl<P: MqttPublisher> ManagedHostStateRepublisher<P> {
    /// `ManagedHostStateRepublisher::new` reuses
    /// `carbide_dsx_event_bus_publish_count_total`. Its
    /// `component="managed_host_republish"` label keeps periodic sweeps
    /// distinct from change-driven publishes on dashboards.
    pub(crate) fn new(publisher: P, params: ManagedHostStateRepublisherParams) -> Self {
        let metrics = MqttHookMetrics::without_queue_depth(PublishComponent::ManagedHostRepublish);
        Self {
            publisher,
            db_pool: params.db_pool,
            work_lock_manager_handle: params.work_lock_manager_handle,
            topic_prefix: params.topic_prefix,
            publish_timeout: params.publish_timeout,
            config: params.config,
            host_health_config: params.host_health_config,
            metrics,
        }
    }

    /// Spawn the republisher's background sweep loop into `join_set` when
    /// enabled. A no-op when `config.enabled` is false.
    pub(crate) fn start(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> std::io::Result<()> {
        if self.config.enabled {
            tracing::info!(
                interval_seconds = self.config.publish_interval().as_secs(),
                scope = ?self.config.scope,
                healthy_republish_interval_sweeps = self.config.healthy_republish_every,
                max_publishes_per_second = self.config.max_publishes_per_second.0,
                "Starting periodic managed host state republisher"
            );
            join_set
                .build_task()
                .name("managed_host_state_republisher")
                .spawn(async move { self.run(cancel_token).await })?;
        }

        Ok(())
    }

    async fn run(self, cancel_token: CancellationToken) {
        // A ticker (rather than `sleep(interval)` after each sweep) keeps the
        // cadence fixed regardless of how long a sweep's publishes take. The
        // first tick fires immediately, so the first sweep runs at startup. If
        // a sweep overruns the interval, missed ticks are skipped rather than
        // bursting back-to-back sweeps.
        let mut ticker = tokio::time::interval(self.config.publish_interval());
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut sweep: u64 = 0;

        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = cancel_token.cancelled() => {
                    tracing::debug!("Managed host state republisher stop requested");
                    return;
                }
            }

            let publish_healthy = should_publish_healthy(
                self.config.scope,
                self.config.healthy_republish_every,
                sweep,
            );
            let result = self.run_sweep(publish_healthy, &cancel_token).await;
            managed_loop::record_iteration(LoopManager::ManagedHostStateRepublisher, &result);
            sweep = sweep.wrapping_add(1);
        }
    }

    /// Page through every managed host and re-publish those selected by the
    /// current scope. Unhealthy hosts are always published; healthy hosts are
    /// published only when `publish_healthy` is true for this sweep.
    ///
    /// The full ID list is fetched cheaply up front (IDs only, no per-host
    /// snapshot JSON). Each host snapshot is then loaded immediately before its
    /// publish, keeping stale data from sitting in memory while paced publishes
    /// drain.
    async fn run_sweep(
        &self,
        publish_healthy: bool,
        cancel_token: &CancellationToken,
    ) -> eyre::Result<()> {
        let _work_lock = match self
            .work_lock_manager_handle
            .try_acquire_lock(REPUBLISH_WORK_KEY.into())
            .await
        {
            Ok(lock) => lock,
            Err(AcquireLockError::WorkAlreadyLocked(_)) => {
                tracing::debug!(
                    lock = REPUBLISH_WORK_KEY,
                    "Skipping managed host state republish sweep; another instance holds the lock"
                );
                return Ok(());
            }
            Err(e) => {
                return Err(eyre::Report::new(e).wrap_err(format!(
                    "unable to acquire managed host state republish lock `{REPUBLISH_WORK_KEY}`"
                )));
            }
        };

        let host_ids = db::managed_host::load_host_ids(&self.db_pool).await?;
        let total = host_ids.len();

        let mut pacing = self
            .config
            .max_publishes_per_second
            .pacing_delay()
            .map(|period| {
                let mut ticker = tokio::time::interval_at(Instant::now() + period, period);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                ticker
            });
        let mut reader: db::db_read::PgPoolReader = self.db_pool.clone().into();
        let mut published = 0usize;
        let mut skipped_healthy = 0usize;

        'sweep: for machine_id in host_ids {
            if cancel_token.is_cancelled() {
                break;
            }

            // Instance data isn't needed: the message only carries `machine_id`
            // and `managed_state` (the host's own state column); aggregate
            // health is derived from host + DPU snapshots.
            let options = LoadSnapshotOptions {
                include_history: false,
                include_instance_data: false,
                host_health_config: self.host_health_config,
            };

            // A host can be deleted between the ID load and the per-host
            // snapshot read.
            let Some(snapshot) =
                db::managed_host::load_snapshot(&mut reader, &machine_id, options).await?
            else {
                continue;
            };

            let unhealthy = is_report_unhealthy(&snapshot.aggregate_health);
            if !should_publish(unhealthy, publish_healthy) {
                skipped_healthy += 1;
                continue;
            }

            publish_state(
                &self.publisher,
                &self.topic_prefix,
                self.publish_timeout,
                &self.metrics,
                &machine_id,
                &snapshot.managed_state,
                Utc::now(),
            )
            .await;
            published += 1;

            if let Some(ticker) = &mut pacing {
                tokio::select! {
                    _ = ticker.tick() => {}
                    _ = cancel_token.cancelled() => break 'sweep,
                }
            }
        }

        tracing::info!(
            total_host_count = total,
            published_host_count = published,
            skipped_healthy_host_count = skipped_healthy,
            publish_healthy,
            scope = ?self.config.scope,
            "Managed host state republish sweep complete"
        );

        Ok(())
    }
}

/// Whether healthy hosts should be published on a given sweep (0-indexed).
///
/// `UnhealthyOnly` never publishes healthy hosts. `All` publishes them every
/// `healthy_republish_every` sweeps (sweep 0 always publishes); `0` is treated
/// as `1`.
fn should_publish_healthy(scope: RepublishScope, healthy_republish_every: u32, sweep: u64) -> bool {
    match scope {
        RepublishScope::UnhealthyOnly => false,
        RepublishScope::All => {
            let every = u64::from(healthy_republish_every.max(1));
            sweep.is_multiple_of(every)
        }
    }
}

/// Whether a host should be published this sweep: unhealthy hosts always are;
/// healthy hosts only when `publish_healthy` is set for the sweep.
fn should_publish(unhealthy: bool, publish_healthy: bool) -> bool {
    unhealthy || publish_healthy
}

/// A managed host is "unhealthy" when its aggregate health carries at least one
/// alert.
fn is_report_unhealthy(report: &HealthReport) -> bool {
    !report.alerts.is_empty()
}

/// Serialize and publish a single managed host's current state, bounded by
/// `publish_timeout`, recording the outcome in `metrics`.
async fn publish_state<P: MqttPublisher>(
    publisher: &P,
    topic_prefix: &str,
    publish_timeout: Duration,
    metrics: &MqttHookMetrics,
    machine_id: &carbide_uuid::machine::HostMachineId,
    state: &ManagedHostState,
    timestamp: DateTime<Utc>,
) {
    let message = ManagedHostStateMessage {
        machine_id,
        managed_host_state: state,
        timestamp,
    };
    let topic = message.topic(topic_prefix);
    let machine_id_context = machine_id.to_string();

    let payload = match message.to_json_bytes() {
        Ok(payload) => payload,
        Err(error) => {
            metrics.record_republish_serialization_error(
                topic,
                machine_id_context,
                error.to_string(),
            );
            return;
        }
    };

    // Same topic layout and publish/timeout/metrics handling as the
    // change-driven hook, shared so the two paths can't drift.
    let deadline = Instant::now() + publish_timeout;
    publish_with_deadline(
        publisher,
        &topic,
        &machine_id_context,
        payload,
        deadline,
        metrics,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use mqttea::MqtteaClientError;
    use tokio::sync::Barrier;

    use super::*;
    use crate::cfg::file::PublishRate;

    fn test_metrics() -> MqttHookMetrics {
        MqttHookMetrics::without_queue_depth(PublishComponent::ManagedHostRepublish)
    }

    fn test_machine_id() -> carbide_uuid::machine::HostMachineId {
        carbide_uuid::machine::MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0; 32],
            MachineType::Host,
        )
        .try_into()
        .unwrap()
    }

    /// Publisher that forwards each published (topic, payload) over a channel.
    struct SignalingPublisher {
        sender: tokio::sync::mpsc::UnboundedSender<(String, Vec<u8>)>,
    }

    impl SignalingPublisher {
        fn new() -> (
            Self,
            tokio::sync::mpsc::UnboundedReceiver<(String, Vec<u8>)>,
        ) {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            (Self { sender }, receiver)
        }
    }

    #[async_trait::async_trait]
    impl MqttPublisher for SignalingPublisher {
        async fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), MqtteaClientError> {
            let _ = self.sender.send((topic.to_string(), payload));
            Ok(())
        }
    }

    #[test]
    fn unhealthy_only_never_publishes_healthy() {
        for sweep in 0..5 {
            assert!(!should_publish_healthy(
                RepublishScope::UnhealthyOnly,
                1,
                sweep
            ));
        }
    }

    #[test]
    fn all_scope_every_one_publishes_healthy_every_sweep() {
        for sweep in 0..5 {
            assert!(should_publish_healthy(RepublishScope::All, 1, sweep));
        }
    }

    #[test]
    fn all_scope_zero_cadence_is_treated_as_one() {
        for sweep in 0..5 {
            assert!(should_publish_healthy(RepublishScope::All, 0, sweep));
        }
    }

    #[test]
    fn all_scope_every_three_publishes_healthy_on_multiples() {
        let got: Vec<bool> = (0..7)
            .map(|sweep| should_publish_healthy(RepublishScope::All, 3, sweep))
            .collect();
        assert_eq!(
            got,
            vec![true, false, false, true, false, false, true],
            "healthy hosts publish on sweeps 0, 3, 6"
        );
    }

    #[test]
    fn unhealthy_hosts_always_publish_regardless_of_healthy_flag() {
        assert!(should_publish(true, false));
        assert!(should_publish(true, true));
    }

    #[test]
    fn healthy_hosts_publish_only_when_flag_set() {
        assert!(!should_publish(false, false));
        assert!(should_publish(false, true));
    }

    #[test]
    fn report_with_no_alerts_is_healthy() {
        assert!(!is_report_unhealthy(&HealthReport::empty(
            "test".to_string()
        )));
    }

    #[test]
    fn report_with_an_alert_is_unhealthy() {
        // `missing_report` carries a single alert.
        assert!(is_report_unhealthy(&HealthReport::missing_report()));
    }

    #[test]
    fn pacing_disabled_when_zero() {
        assert_eq!(PublishRate(0).pacing_delay(), None);
    }

    #[test]
    fn pacing_divides_one_second_by_rate() {
        assert_eq!(
            PublishRate(10).pacing_delay(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(PublishRate(1).pacing_delay(), Some(Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn publish_state_uses_state_topic_and_payload() {
        let (publisher, mut receiver) = SignalingPublisher::new();
        let metrics = test_metrics();
        let id = test_machine_id();
        let state = ManagedHostState::Ready;

        publish_state(
            &publisher,
            "NICO/v1/machine",
            Duration::from_secs(1),
            &metrics,
            &id,
            &state,
            Utc::now(),
        )
        .await;

        let (topic, payload) = receiver.recv().await.expect("should receive message");
        assert_eq!(topic, format!("NICO/v1/machine/{id}/state"));

        let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(
            parsed
                .get("managed_host_state")
                .unwrap()
                .get("state")
                .unwrap(),
            "ready"
        );
        assert!(parsed.get("machine_id").is_some());
        assert!(parsed.get("timestamp").is_some());
    }

    #[tokio::test]
    async fn publish_state_respects_publish_timeout() {
        let started = Arc::new(Barrier::new(2));
        let call_count = Arc::new(AtomicUsize::new(0));
        let complete_count = Arc::new(AtomicUsize::new(0));

        struct TimeoutPublisher {
            started: Arc<Barrier>,
            call_count: Arc<AtomicUsize>,
            complete_count: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl MqttPublisher for TimeoutPublisher {
            async fn publish(&self, _: &str, _: Vec<u8>) -> Result<(), MqtteaClientError> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                self.started.wait().await;
                std::future::pending::<()>().await;
                self.complete_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let publisher = TimeoutPublisher {
            started: started.clone(),
            call_count: call_count.clone(),
            complete_count: complete_count.clone(),
        };
        let metrics = test_metrics();
        let id = test_machine_id();
        let state = ManagedHostState::Ready;

        let publish = publish_state(
            &publisher,
            "NICO/v1/machine",
            Duration::from_millis(10),
            &metrics,
            &id,
            &state,
            Utc::now(),
        );

        // The publish returns once the timeout fires; the inner publish never
        // completes (it is parked on `pending`).
        tokio::join!(publish, async {
            started.wait().await;
        });

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert_eq!(complete_count.load(Ordering::SeqCst), 0);
    }
}
