// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Poll pool statistics every minute and keep the last successful observation.
//! Availability describes the last attempt; freshness describes whether the
//! last success is less than two minutes old. Count metrics require both, so
//! a failed or stalled poll cannot leave old capacity looking current.
//! Observations include local inventory and saved backend bindings, plus known
//! empty counts for common and IB pools without local inventory.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_instrument::{Event, LabelValue, emit};
use carbide_utils::metrics::SharedMetricsHolder;
use futures::StreamExt;
use model::resource_pool::common::CommonPools;
use rpc::forge as rpc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::ResourcePoolMonitor;

const POLL_INTERVAL: Duration = Duration::from_secs(60);
const FRESH_PERIOD: Duration = Duration::from_secs(120);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// `PoolStats` shares completed observations with the service health exporter.
pub(crate) type PoolStats = Arc<Mutex<HashMap<String, StatsObservation>>>;

/// `StatsObservation` keeps failed attempts separate from the last good counts.
/// Failures leave that sample and its original `observed_at` unchanged.
#[derive(Debug)]
pub(crate) struct StatsObservation {
    /// Status from the last completed attempt, including unsupported statistics.
    pub(crate) latest_status: rpc::ResourcePoolStatsStatus,
    /// Last successful counts, with a `fresh` attribute supplied by the holder.
    pub(crate) last_success: SharedMetricsHolder<rpc::ResourcePoolInspection>,
}

impl Default for StatsObservation {
    fn default() -> Self {
        Self {
            latest_status: rpc::ResourcePoolStatsStatus::Unspecified,
            last_success: SharedMetricsHolder::with_fresh_period(FRESH_PERIOD),
        }
    }
}

impl StatsObservation {
    /// `update` refreshes counts only when the backend returned known statistics.
    fn update(&mut self, sample: rpc::ResourcePoolInspection) {
        self.latest_status = sample.stats_status();
        if self.latest_status == rpc::ResourcePoolStatsStatus::Known {
            self.last_success.update(sample);
        }
    }
}

/// Which implementation could not supply statistics; never a configured name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum StatsBackend {
    /// The local database snapshot, including its saved remote bindings.
    Integrated,
    /// One pool selected through a gRPC backend.
    Grpc,
}

/// Bounded reasons for a failed database snapshot or individual observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum StatsFailure {
    /// The local inventory/binding snapshot returned a database error.
    Database,
    /// The local snapshot exceeded its total read budget.
    Timeout,
    /// The provider does not implement statistics.
    Unknown,
    /// A pool returned no usable statistics, including provider/read timeouts.
    Unavailable,
}

/// Counts failed polls while keeping pool names and error details in the log.
#[derive(Event)]
#[event(
    event_name = "resource_pool_stats_poll_failed",
    metric_name = "carbide_resourcepool_stats_poll_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "Could not read resource pool statistics",
    describe = "Number of failed resource pool observations: one per unavailable or unsupported pool, or per failed database snapshot. Labels: backend (integrated for the local snapshot, grpc for a remote pool), failure (database or timeout for snapshots, unknown for unsupported statistics, unavailable for other pool failures)"
)]
struct StatsPollFailed {
    #[label]
    backend: StatsBackend,
    #[label]
    failure: StatsFailure,
    #[context]
    pool_name: Option<String>,
    #[context]
    error: String,
}

/// `start_pool_stats` starts an immediate poll, then polls every sixty seconds.
/// Database snapshots have a five-second limit; remote calls use the monitor's
/// bounded scheduling and client deadlines. Each completed row is published
/// immediately, without waiting for a slower provider.
///
/// The caller cancels and joins this task with the rest of the API. Cancellation
/// drops pending SQL and RPC futures. No counts or success timestamp are exposed
/// before a successful observation, and samples become stale after 120 seconds.
/// A credential file read already running in a blocking worker can outlive this
/// task; see [`carbide_resource_pool::grpc::GrpcResourcePoolBackend::new`].
pub(crate) fn start_pool_stats(
    monitor: ResourcePoolMonitor,
    common_pools: &CommonPools,
    tasks: &mut JoinSet<()>,
    cancel: CancellationToken,
) -> std::io::Result<PoolStats> {
    let ethernet = &common_pools.ethernet;
    let mut common_names: Vec<String> = [
        ethernet.pool_loopback_ip.name(),
        ethernet.pool_loopback_ip_v6.name(),
        ethernet.pool_vlan_id.name(),
        ethernet.pool_vni.name(),
        ethernet.pool_vpc_vni.name(),
        ethernet.pool_external_vpc_vni.name(),
        ethernet.pool_dpa_vni.name(),
        ethernet.pool_fnn_asn.name(),
        ethernet.pool_vpc_dpu_loopback_ip.name(),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    common_names.extend(
        common_pools
            .infiniband
            .pkey_pools
            .values()
            .map(|pool| pool.name().to_owned()),
    );
    let stats: PoolStats = Arc::new(Mutex::new(
        common_names
            .iter()
            .map(|name| (name.clone(), StatsObservation::default()))
            .collect(),
    ));
    let observed = stats.clone();
    tasks
        .build_task()
        .name("resource_pool_stats")
        .spawn(async move {
            let mut interval = tokio::time::interval(POLL_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while cancel.run_until_cancelled(interval.tick()).await.is_some() {
                if cancel
                    .run_until_cancelled(poll(&monitor, &observed, &common_names))
                    .await
                    .is_none()
                {
                    return;
                }
            }
        })?;
    Ok(stats)
}

/// `poll` publishes rows as they complete, retaining successful samples on error.
async fn poll(monitor: &ResourcePoolMonitor, stats: &PoolStats, common_names: &[String]) {
    let observations = tokio::time::timeout(SNAPSHOT_TIMEOUT, monitor.observe(common_names))
        .await
        .map_err(|error| (StatsFailure::Timeout, error.to_string()))
        .and_then(|result| result.map_err(|error| (StatsFailure::Database, error.to_string())));
    let observations = match observations {
        Ok(observations) => observations,
        Err((failure, error)) => {
            for observation in stats.lock().unwrap().values_mut() {
                observation.latest_status = rpc::ResourcePoolStatsStatus::Unavailable;
            }
            emit(StatsPollFailed {
                backend: StatsBackend::Integrated,
                failure,
                pool_name: None,
                error,
            });
            return;
        }
    };

    let mut seen = HashSet::new();
    futures::pin_mut!(observations);
    while let Some(sample) = observations.next().await {
        let failure = match sample.stats_status() {
            rpc::ResourcePoolStatsStatus::Known => None,
            rpc::ResourcePoolStatsStatus::Unknown => Some(StatsFailure::Unknown),
            rpc::ResourcePoolStatsStatus::Unavailable
            | rpc::ResourcePoolStatsStatus::Unspecified => Some(StatsFailure::Unavailable),
        };
        if let Some(failure) = failure {
            emit(StatsPollFailed {
                backend: match sample.backend_kind() {
                    rpc::ResourcePoolBackendKind::Grpc => StatsBackend::Grpc,
                    rpc::ResourcePoolBackendKind::Integrated
                    | rpc::ResourcePoolBackendKind::Unspecified => StatsBackend::Integrated,
                },
                failure,
                pool_name: Some(sample.name.clone()),
                error: sample
                    .stats_message
                    .clone()
                    .unwrap_or_else(|| "statistics unavailable".to_owned()),
            });
        }
        seen.insert(sample.name.clone());
        stats
            .lock()
            .unwrap()
            .entry(sample.name.clone())
            .or_default()
            .update(sample);
    }

    // Remove disappeared pools only after the complete pass. Expected local
    // pools and failed remote observations both have their own rows in `seen`.
    stats.lock().unwrap().retain(|name, _| seen.contains(name));
}
