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

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{MachineId, MachineType};
use futures_util::StreamExt;
use futures_util::future::join_all;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Meter, ObservableGauge};
use rpc::carbide_api_client::CarbideApiClient;
use rpc::forge;
use tokio::sync::oneshot::Receiver;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::bmc::client::{BmcConnectionSubscription, BmcRecoveryAttempted, ClientHandle};
use crate::bmc::connection::{ConnectionDetails, LookupError, State};
use crate::bmc::{client, connection};
use crate::config::Config;
use crate::fork_cancel_token;
use crate::shutdown_handle::{ReadyHandle, ShutdownHandle};
use crate::ssh_server::ServerMetrics;

/// Connection-detail lookups in flight during one refresh.
const LOOKUP_CONCURRENCY: usize = 64;

/// Spawn a background task that keeps a bounded pool of BMC connections.
///
/// Up to `max_bmc_connections` machines from the API get a standing connection for console
/// logging. Any machine without one connects when a console session asks for it, replacing the
/// pooled connection that has been without a session the longest if the pool is full.
pub(crate) fn spawn(
    config: Arc<Config>,
    carbide_api_client: CarbideApiClient,
    meter: &Meter,
    instrumentation: carbide_instrument::Instrumentation,
    cancel_token: CancellationToken,
) -> Handle {
    let (cancel_token, drop_guard) = fork_cancel_token(cancel_token);
    let (ready_tx, ready_rx) = oneshot::channel();
    let members: Members = Default::default();
    let pool = Arc::new(BmcPool {
        members: members.clone(),
        permits: Arc::new(Semaphore::new(
            config.max_bmc_connections.get().min(Semaphore::MAX_PERMITS),
        )),
        probe_permits: Arc::new(Semaphore::new(
            config
                .max_concurrent_bmc_probes
                .get()
                .min(Semaphore::MAX_PERMITS),
        )),
        metrics: Arc::new(BmcPoolMetrics::new(meter, instrumentation, members)),
        config,
        carbide_api_client,
        cancel_token,
    });
    let join_handle = tokio::spawn(pool.clone().run_loop(ready_tx));

    Handle {
        connection_store: BmcConnectionStore(pool),
        ready_rx: Some(ready_rx),
        join_handle,
        drop_guard,
    }
}

/// A owned handle to the entire BMC client pool background task. The pool will shut down when this is
/// dropped.
pub(crate) struct Handle {
    connection_store: BmcConnectionStore,
    ready_rx: Option<oneshot::Receiver<()>>,
    join_handle: tokio::task::JoinHandle<()>,
    drop_guard: DropGuard,
}

impl Handle {
    pub(crate) fn connection_store(&self) -> BmcConnectionStore {
        self.connection_store.clone()
    }
}

impl ShutdownHandle<()> for Handle {
    fn into_parts(self) -> (DropGuard, JoinHandle<()>) {
        (self.drop_guard, self.join_handle)
    }
}

impl ReadyHandle for Handle {
    fn take_ready_rx(&mut self) -> Option<Receiver<()>> {
        self.ready_rx.take()
    }
}

type Members = Arc<RwLock<HashMap<MachineId, Member>>>;

/// A pooled BMC client and the pool permit it holds.
struct Member {
    client: ClientHandle,
    origin: Origin,
    _permit: OwnedSemaphorePermit,
}

impl Member {
    fn view(&self) -> MemberView {
        MemberView {
            origin: self.origin,
            idle_since: self.client.idle_since(),
        }
    }
}

/// Why a client is in the pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    /// Kept open for console logging while the machine is in the API's machine list.
    Standing,
    /// Opened for a console session; closed once idle for `on_demand_bmc_idle_timeout`.
    OnDemand,
}

/// The part of a member that the pool's eviction rules read.
#[derive(Clone, Copy, Debug)]
struct MemberView {
    origin: Origin,
    /// Since when the member has had no console session, or `None` while one is open.
    idle_since: Option<Instant>,
}

/// On-demand members whose last console session ended at least `timeout` ago.
fn expired_on_demand(
    members: impl Iterator<Item = (MachineId, MemberView)>,
    now: Instant,
    timeout: Duration,
) -> Vec<MachineId> {
    members
        .filter(|(_, member)| member.origin == Origin::OnDemand)
        .filter(|(_, member)| {
            member
                .idle_since
                .is_some_and(|idle_since| now.saturating_duration_since(idle_since) >= timeout)
        })
        .map(|(machine_id, _)| machine_id)
        .collect()
}

/// The member to close when a full pool needs room: the one without a console session that has
/// been idle the longest.
fn preemption_victim(members: impl Iterator<Item = (MachineId, MemberView)>) -> Option<MachineId> {
    members
        .filter_map(|(machine_id, member)| member.idle_since.map(|since| (since, machine_id)))
        .min()
        .map(|(_, machine_id)| machine_id)
}

/// Whether this instance serves consoles for machines of this type.
fn console_enabled_for(config: &Config, machine_type: MachineType) -> bool {
    match machine_type {
        MachineType::Host => config.hosts,
        MachineType::Dpu => config.dpus,
        MachineType::PredictedHost => false,
    }
}

/// A shared reference to the pool, used by frontends to reach BMC connections.
#[derive(Clone)]
pub(crate) struct BmcConnectionStore(Arc<BmcPool>);

impl BmcConnectionStore {
    pub(crate) fn console_log_client(
        &self,
        machine_id: &MachineId,
    ) -> Option<Option<crate::console_logger::ConsoleLogClient>> {
        self.0
            .members
            .read()
            .expect("lock poisoned")
            .get(machine_id)
            .map(|member| member.client.console_log_client())
    }

    /// Subscribe to the BMC of a machine or instance, connecting to it on demand when it has no
    /// pooled connection.
    pub(crate) async fn get_connection(
        &self,
        machine_or_instance_id: &str,
        metrics: Arc<ServerMetrics>,
    ) -> Result<BmcConnectionSubscription, GetConnectionError> {
        let machine_id = self.resolve_machine_id(machine_or_instance_id).await?;
        self.0.connect(machine_id, metrics).await
    }

    async fn resolve_machine_id(
        &self,
        machine_or_instance_id: &str,
    ) -> Result<MachineId, GetConnectionError> {
        if let Ok(machine_id) = MachineId::from_str(machine_or_instance_id) {
            return Ok(machine_id);
        }
        let Ok(instance_id) = InstanceId::from_str(machine_or_instance_id) else {
            return Err(GetConnectionError::InvalidMachineId {
                machine_or_instance_id: machine_or_instance_id.to_owned(),
            });
        };
        if let Some(machine_id) = self
            .0
            .config
            .override_bmcs
            .iter()
            .flatten()
            .find_map(|bmc| {
                if bmc
                    .instance_id
                    .as_ref()
                    .is_some_and(|i| i.eq(machine_or_instance_id))
                {
                    bmc.machine_id
                        .parse()
                        .inspect_err(|error| {
                            tracing::warn!(
                                machine_id = bmc.machine_id,
                                %error,
                                "invalid machine_id in bmc override config"
                            );
                        })
                        .ok()
                } else {
                    None
                }
            })
        {
            return Ok(machine_id);
        }
        let machine_id = self
            .0
            .carbide_api_client
            .find_instances_by_ids(forge::InstancesByIdsRequest {
                instance_ids: vec![instance_id],
            })
            .await
            .map_err(|e| GetConnectionError::InstanceIdLookupFailure {
                tonic_status: e,
                instance_id,
            })?
            .instances
            .into_iter()
            .next()
            .ok_or_else(|| GetConnectionError::CouldNotFindInstanceId { instance_id })?
            .machine_id
            .ok_or_else(|| GetConnectionError::InstanceMissingMachineId { instance_id })?;
        Ok(machine_id.into())
    }
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum GetConnectionError {
    #[error("{machine_or_instance_id} is not a valid machine_id or instance ID")]
    InvalidMachineId { machine_or_instance_id: String },
    #[error("error looking up instance ID {instance_id}: {tonic_status}")]
    InstanceIdLookupFailure {
        instance_id: InstanceId,
        tonic_status: tonic::Status,
    },
    #[error("could not find instance with id {instance_id}")]
    CouldNotFindInstanceId { instance_id: InstanceId },
    #[error("instance {instance_id} has no machine ID")]
    InstanceMissingMachineId { instance_id: InstanceId },
    #[error("consoles for {machine_type:?} machines are disabled, refusing {machine_id}")]
    ConsoleDisabled {
        machine_id: MachineId,
        machine_type: MachineType,
    },
    #[error("error looking up BMC connection details for {machine_id}: {error}")]
    Lookup {
        machine_id: MachineId,
        error: LookupError,
    },
    #[error(
        "BMC connection pool is full and every connection has an open console session, cannot connect to {machine_id}"
    )]
    PoolFull { machine_id: MachineId },
}

/// The BMC connection pool: one client per pooled machine, bounded by `permits`.
struct BmcPool {
    members: Members,
    /// One permit per pooled connection.
    permits: Arc<Semaphore>,
    /// One permit per reachability probe in flight, shared by every client.
    probe_permits: Arc<Semaphore>,
    config: Arc<Config>,
    carbide_api_client: CarbideApiClient,
    metrics: Arc<BmcPoolMetrics>,
    cancel_token: CancellationToken,
}

pub(super) struct BmcPoolMetrics {
    pub(super) instrumentation: carbide_instrument::Instrumentation,
    grpc_total_hosts: Gauge<u64>,
    total_machines: Gauge<u64>,
    _failed_machines: ObservableGauge<u64>,
    _healthy_machines: ObservableGauge<u64>,
    _bmc_status: ObservableGauge<u64>,

    // Updated by the BMC client and connection workers.
    pub(super) bmc_bytes_received_total: Counter<u64>,
    pub(super) bmc_rx_errors_total: Counter<u64>,
    pub(super) bmc_tx_errors_total: Counter<u64>,
    pub(super) bmc_recovery_attempts: Gauge<u64>,
}

impl BmcPoolMetrics {
    fn new(
        meter: &Meter,
        instrumentation: carbide_instrument::Instrumentation,
        members: Members,
    ) -> Self {
        // Expose the zero series in this pool's registry before any BMC client connects.
        let initialized = instrumentation.initialize_counter_series(&BmcRecoveryAttempted);
        debug_assert!(
            initialized,
            "BMC recovery Event must initialize its counter"
        );
        Self {
            instrumentation,
            grpc_total_hosts: meter
                .u64_gauge("ssh_console_grpc_total_machines")
                .with_description("Number of hosts reported by the Site Controller to the SSH Console service").build(),
            total_machines: meter
                .u64_gauge("ssh_console_total_machines")
                .with_description("Number of host BMCs the SSH Console service has attempted to connect to").build(),
            _failed_machines: meter
                .u64_observable_gauge("ssh_console_failed_machines")
                .with_description("Number of host BMCs with connection errors")
                .with_callback({
                    let members = members.clone();
                    move |observer| {
                        let error_count = members.read().expect("lock poisoned")
                            .values()
                            .fold(0, |acc, member| {
                                if member.client.connection_state.load() == State::ConnectionError { acc + 1 } else { acc }
                            });
                        observer.observe(error_count, &[]);
                    }
                })
                .build(),
            _healthy_machines: meter
                .u64_observable_gauge("ssh_console_healthy_machines")
                .with_description("Number of host BMCs the SSH Console service has working connections to")
                .with_callback({
                    let members = members.clone();
                    move |observer| {
                        let error_count = members.read().expect("lock poisoned")
                            .values()
                            .fold(0, |acc, member| {
                                if member.client.connection_state.load() == State::Connected { acc + 1 } else { acc }
                            });
                        observer.observe(error_count, &[]);
                    }
                })
                .build(),
            _bmc_status: meter.u64_observable_gauge("ssh_console_bmc_status")
                .with_description("Current status of the session to the bmc, see value label")
                .with_callback({
                    move |observer| {
                        members.read().expect("lock poisoned").iter().for_each(|(machine_id, member)| {
                            let state = member.client.connection_state.load();
                            observer.observe(1, &[
                                KeyValue::new("machine_id", machine_id.to_string()),
                                KeyValue::new("value", format!("{state:?}")),
                            ])
                        })
                    }
                })
                .build(),
            bmc_bytes_received_total: meter
                .u64_counter("ssh_console_bmc_bytes_received")
                .with_description("Total bytes received during this service lifetime from the bmc")
                .with_unit("bytes")
                .build(),
            bmc_rx_errors_total: meter
                .u64_counter("ssh_console_bmc_rx_errors")
                .with_description("Total receive errors encountered during this service lifetime connection with the bmc")
                .build(),
            bmc_tx_errors_total: meter
                .u64_counter("ssh_console_bmc_tx_errors")
                .with_description("Total transmit errors encountered during this service lifetime connection with the bmc")
                .build(),
            bmc_recovery_attempts: meter
                .u64_gauge("ssh_console_bmc_recovery_attempts")
                .with_description("Recovery attempts made for connection or session errors")
                .build(),
        }
    }

    #[cfg(test)]
    pub(super) fn for_test(
        meter: &Meter,
        instrumentation: carbide_instrument::Instrumentation,
    ) -> Self {
        Self::new(meter, instrumentation, Arc::default())
    }
}

impl BmcPool {
    /// Run a loop which refreshes the list of BMC's from the API and keeps a standing connection
    /// to as many of them as the pool bound allows.
    async fn run_loop(self: Arc<Self>, ready_tx: oneshot::Sender<()>) {
        let mut api_refresh = tokio::time::interval(self.config.api_poll_interval);
        // Don't try to catch up if for some reason api refresh takes forever (ie. if the connection
        // is down and we have to retry a long time.)
        api_refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut ready_tx = Some(ready_tx);

        while self
            .cancel_token
            .run_until_cancelled(api_refresh.tick())
            .await
            .is_some()
        {
            if let Err(error) = self.refresh_bmcs().await {
                tracing::error!(%error, "error refreshing BMC list from API");
            }
            // Inform callers that we're ready once the first API refresh happens.
            ready_tx.take().map(|ch| ch.send(()).ok());
        }
        tracing::info!("shutting down BmcPool");

        // Wait for each BMC connection to shut down (their CancellationToken is cloned from ours,
        // so they should be shutting down too.)
        let clients = self
            .members
            .write()
            .expect("lock poisoned")
            .drain()
            .map(|(_machine_id, member)| member.client)
            .collect::<Vec<_>>();
        join_all(clients.into_iter().map(|client| client.shutdown_and_wait())).await;
    }

    async fn refresh_bmcs(&self) -> Result<(), RefreshBmcsError> {
        // Get all machine ID's from forge, parsing them into carbide_uuid::MachineId.
        let machine_ids: HashSet<MachineId> = match &self.config.override_bmcs {
                Some(override_bmcs) => {
                    override_bmcs
                        .iter()
                        .filter_map(|b| {
                            b.machine_id
                                .parse()
                                .inspect_err(|error| {
                                    tracing::error!(
                                    %error,
                                    machine_id = %b.machine_id,
                                    "invalid machine ID in config, will not do console logging on this machine"
                                )
                                }).ok()
                        })
                        .collect()
                }
                None => {
                    self.carbide_api_client
                        .find_machine_ids(forge::MachineSearchConfig {
                            exclude_hosts: !self.config.hosts,
                            include_dpus: self.config.dpus,
                            ..Default::default()
                        })
                        .await
                        .map_err(|e| RefreshBmcsError::FetchingMachineIdsFailure { tonic_status: e })?
                        .machine_ids
                        .into_iter()
                        .collect()
                }
            };

        self.metrics
            .grpc_total_hosts
            .record(machine_ids.len() as _, &[]);

        // -- Reconcile our list with the running tasks
        let to_add = {
            let mut members = self.members.write().expect("lock poisoned");

            // Remove any machines that are no longer monitored
            let to_remove = members
                .keys()
                .filter(|&machine_id| !machine_ids.contains(machine_id))
                .copied()
                .collect::<Vec<_>>();
            for machine_id in to_remove {
                tracing::info!(%machine_id, "removing machine from console logging, no longer found in carbide");
                members.remove(&machine_id);
            }

            // Close on-demand connections nobody has used for the idle timeout
            let now = Instant::now();
            for machine_id in expired_on_demand(
                members.iter().map(|(id, member)| (*id, member.view())),
                now,
                self.config.on_demand_bmc_idle_timeout,
            ) {
                tracing::info!(%machine_id, "closing idle on-demand BMC connection");
                members.remove(&machine_id);
            }

            // Reserve a permit for each machine to add while permits last. Machines left over
            // connect on demand.
            let mut to_add = Vec::new();
            for machine_id in machine_ids.iter().filter(|id| !members.contains_key(id)) {
                let Ok(permit) = self.permits.clone().try_acquire_owned() else {
                    break;
                };
                to_add.push((*machine_id, permit));
            }
            to_add
        };

        // For each one we want to add, get the connection details. Skip any machines which fail
        // here.
        let all_connection_details = futures_util::stream::iter(to_add)
            .map(|(machine_id, permit)| async move {
                match connection::lookup(
                    &machine_id.to_string(),
                    &self.config,
                    &self.carbide_api_client,
                )
                .await
                {
                    Ok(connection_details) => Some((machine_id, connection_details, permit)),
                    Err(error) => {
                        tracing::error!(%machine_id, %error, "error looking up connection details, excluding from bmc list");
                        None
                    }
                }
            })
            .buffer_unordered(LOOKUP_CONCURRENCY)
            .filter_map(std::future::ready)
            .collect::<Vec<_>>()
            .await;

        {
            // Now add each of these to the pool
            let mut members = self.members.write().expect("lock poisoned");
            for (machine_id, connection_details, permit) in all_connection_details {
                if members.contains_key(&machine_id) {
                    continue;
                }
                tracing::info!(%machine_id, "begin connection to machine");
                members.insert(
                    machine_id,
                    Member {
                        client: self.spawn_client(connection_details),
                        origin: Origin::Standing,
                        _permit: permit,
                    },
                );
            }

            self.metrics.total_machines.record(members.len() as _, &[]);

            let unpooled = machine_ids
                .iter()
                .filter(|id| !members.contains_key(id))
                .count();
            if unpooled > 0 && self.permits.available_permits() == 0 {
                tracing::warn!(
                    unpooled_machines = unpooled,
                    max_bmc_connections = self.config.max_bmc_connections.get(),
                    "BMC connection pool is full; machines without a pooled connection connect on demand"
                );
            }
        }

        Ok(())
    }

    /// Subscribe to a machine's BMC, connecting on demand when it has no pooled connection.
    async fn connect(
        &self,
        machine_id: MachineId,
        metrics: Arc<ServerMetrics>,
    ) -> Result<BmcConnectionSubscription, GetConnectionError> {
        if let Some(member) = self.members.read().expect("lock poisoned").get(&machine_id) {
            return Ok(member.client.subscribe(metrics));
        }

        let machine_type = machine_id.machine_type();
        if !console_enabled_for(&self.config, machine_type) {
            return Err(GetConnectionError::ConsoleDisabled {
                machine_id,
                machine_type,
            });
        }
        let connection_details = connection::lookup(
            &machine_id.to_string(),
            &self.config,
            &self.carbide_api_client,
        )
        .await
        .map_err(|error| GetConnectionError::Lookup { machine_id, error })?;

        let mut members = self.members.write().expect("lock poisoned");
        if let Some(member) = members.get(&machine_id) {
            return Ok(member.client.subscribe(metrics));
        }
        let permit = self
            .take_permit(&mut members)
            .ok_or(GetConnectionError::PoolFull { machine_id })?;
        tracing::info!(%machine_id, "begin on-demand connection to machine");
        members.insert(
            machine_id,
            Member {
                client: self.spawn_client(connection_details),
                origin: Origin::OnDemand,
                _permit: permit,
            },
        );
        self.metrics.total_machines.record(members.len() as _, &[]);
        Ok(members[&machine_id].client.subscribe(metrics))
    }

    /// Take a pool permit, closing the connection idle the longest when the pool is full.
    fn take_permit(
        &self,
        members: &mut HashMap<MachineId, Member>,
    ) -> Option<OwnedSemaphorePermit> {
        if let Ok(permit) = self.permits.clone().try_acquire_owned() {
            return Some(permit);
        }
        let victim = preemption_victim(members.iter().map(|(id, member)| (*id, member.view())))?;
        tracing::info!(
            machine_id = %victim,
            "BMC connection pool is full; closing the connection idle the longest"
        );
        members.remove(&victim);
        self.permits.clone().try_acquire_owned().ok()
    }

    fn spawn_client(&self, connection_details: ConnectionDetails) -> ClientHandle {
        client::spawn(
            connection_details,
            self.config.clone(),
            self.metrics.clone(),
            self.probe_permits.clone(),
            self.cancel_token.clone(),
        )
    }
}

#[derive(thiserror::Error, Debug)]
enum RefreshBmcsError {
    #[error("error fetching machine ids: {tonic_status}")]
    FetchingMachineIdsFailure { tonic_status: tonic::Status },
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::num::NonZeroUsize;

    use carbide_test_support::value_scenarios;
    use carbide_uuid::machine::MachineIdSource;
    use tokio::net::TcpListener;

    use super::*;
    use crate::bmc::vendor::{BmcVendor, SshBmcVendor};
    use crate::config::BmcConfig;
    use crate::metrics::MetricsState;

    fn machine(n: u8) -> MachineId {
        MachineId::new(MachineIdSource::Tpm, [n; 32], MachineType::Host)
    }

    fn view(origin: Origin, idle_since: Option<Instant>) -> MemberView {
        MemberView { origin, idle_since }
    }

    #[test]
    fn expired_on_demand_closes_only_idle_on_demand_members() {
        let timeout = Duration::from_secs(600);
        let now = Instant::now() + Duration::from_secs(86_400);
        let (a, b) = (machine(1), machine(2));
        value_scenarios!(
            run = |members: Vec<(MachineId, MemberView)>| {
                expired_on_demand(members.into_iter(), now, timeout)
            };
            "standing members never expire" {
                vec![(a, view(Origin::Standing, Some(now - Duration::from_secs(7200))))] => vec![],
            }
            "an on-demand member with an open session stays" {
                vec![(a, view(Origin::OnDemand, None))] => vec![],
            }
            "an on-demand member idle for the timeout or longer expires" {
                vec![(a, view(Origin::OnDemand, Some(now - timeout)))] => vec![a],
                vec![
                    (a, view(Origin::OnDemand, Some(now - timeout - Duration::from_secs(1)))),
                    (b, view(Origin::OnDemand, Some(now - timeout + Duration::from_secs(1)))),
                ] => vec![a],
            }
        );
    }

    #[test]
    fn preemption_victim_is_the_member_idle_the_longest() {
        let now = Instant::now() + Duration::from_secs(86_400);
        let (a, b) = (machine(1), machine(2));
        value_scenarios!(
            run = |members: Vec<(MachineId, MemberView)>| preemption_victim(members.into_iter());
            "an empty pool has no victim" {
                vec![] => None,
            }
            "members with open sessions are never closed" {
                vec![
                    (a, view(Origin::Standing, None)),
                    (b, view(Origin::OnDemand, None)),
                ] => None,
                vec![
                    (a, view(Origin::Standing, None)),
                    (b, view(Origin::Standing, Some(now))),
                ] => Some(b),
            }
            "the longest idle member is closed regardless of origin" {
                vec![
                    (a, view(Origin::OnDemand, Some(now - Duration::from_secs(1)))),
                    (b, view(Origin::Standing, Some(now - Duration::from_secs(2)))),
                ] => Some(b),
                vec![
                    (a, view(Origin::Standing, Some(now - Duration::from_secs(2)))),
                    (b, view(Origin::OnDemand, Some(now - Duration::from_secs(1)))),
                ] => Some(a),
            }
        );
    }

    #[test]
    fn console_enabled_for_follows_the_hosts_and_dpus_settings() {
        let config = |hosts, dpus| Config {
            hosts,
            dpus,
            ..Config::default()
        };
        value_scenarios!(
            run = |(hosts, dpus, machine_type)| console_enabled_for(&config(hosts, dpus), machine_type);
            "hosts follow the hosts setting" {
                (true, false, MachineType::Host) => true,
                (false, true, MachineType::Host) => false,
            }
            "DPUs follow the dpus setting" {
                (false, true, MachineType::Dpu) => true,
                (true, false, MachineType::Dpu) => false,
            }
            "predicted hosts have no BMC" {
                (true, true, MachineType::PredictedHost) => false,
            }
        );
    }

    /// A pool over machines whose BMC ports are closed, so clients wait for them to listen.
    struct TestPool {
        pool: Arc<BmcPool>,
        machines: Vec<MachineId>,
        server_metrics: Arc<ServerMetrics>,
        _metrics_state: MetricsState,
    }

    impl TestPool {
        async fn new(max_connections: usize, idle_timeout: Duration, machine_count: u8) -> Self {
            let machines: Vec<MachineId> = (1..=machine_count).map(machine).collect();
            let mut override_bmcs = Vec::new();
            for machine_id in &machines {
                // Bind and release a port so that nothing listens on it.
                let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .await
                    .expect("bind")
                    .local_addr()
                    .expect("local addr")
                    .port();
                override_bmcs.push(BmcConfig {
                    machine_id: machine_id.to_string(),
                    instance_id: None,
                    ip: Ipv4Addr::LOCALHOST.into(),
                    port: Some(port),
                    user: "test".to_string(),
                    password: "test".to_string(),
                    ssh_key_path: None,
                    bmc_vendor: BmcVendor::Ssh(SshBmcVendor::Dell),
                });
            }
            let config = Arc::new(Config {
                override_bmcs: Some(override_bmcs),
                console_logging_enabled: false,
                max_bmc_connections: NonZeroUsize::new(max_connections).expect("nonzero"),
                on_demand_bmc_idle_timeout: idle_timeout,
                ..Config::default()
            });
            let members: Members = Default::default();
            let metrics_state = MetricsState::new();
            let pool = Arc::new(BmcPool {
                members: members.clone(),
                permits: Arc::new(Semaphore::new(max_connections)),
                probe_permits: Arc::new(Semaphore::new(config.max_concurrent_bmc_probes.get())),
                metrics: Arc::new(BmcPoolMetrics::new(
                    &metrics_state.meter,
                    metrics_state.instrumentation.clone(),
                    members,
                )),
                carbide_api_client: config.make_carbide_api_client(),
                config,
                cancel_token: CancellationToken::new(),
            });
            Self {
                pool,
                machines,
                server_metrics: Arc::new(ServerMetrics::for_test()),
                _metrics_state: metrics_state,
            }
        }

        async fn refresh(&self) {
            self.pool.refresh_bmcs().await.expect("refresh");
        }

        async fn connect(
            &self,
            machine_id: MachineId,
        ) -> Result<BmcConnectionSubscription, GetConnectionError> {
            self.pool
                .connect(machine_id, self.server_metrics.clone())
                .await
        }

        fn members(&self) -> Vec<(MachineId, Origin)> {
            let mut members: Vec<_> = self
                .pool
                .members
                .read()
                .expect("lock poisoned")
                .iter()
                .map(|(id, member)| (*id, member.origin))
                .collect();
            members.sort_by_key(|(machine_id, _)| *machine_id);
            members
        }

        fn unpooled_machine(&self) -> MachineId {
            let members = self.pool.members.read().expect("lock poisoned");
            self.machines
                .iter()
                .copied()
                .find(|machine_id| !members.contains_key(machine_id))
                .expect("a machine without a pooled connection")
        }

        async fn shutdown(self) {
            self.pool.cancel_token.cancel();
            let clients: Vec<_> = self
                .pool
                .members
                .write()
                .expect("lock poisoned")
                .drain()
                .map(|(_, member)| member.client)
                .collect();
            join_all(clients.into_iter().map(ClientHandle::shutdown_and_wait)).await;
        }
    }

    #[tokio::test]
    async fn standing_connections_stop_at_the_pool_bound() {
        let pool = TestPool::new(2, Duration::from_secs(600), 3).await;
        pool.refresh().await;

        let members = pool.members();
        assert_eq!(members.len(), 2);
        assert!(
            members
                .iter()
                .all(|(_, origin)| *origin == Origin::Standing)
        );
        assert_eq!(pool.pool.permits.available_permits(), 0);

        // A second refresh does not grow the pool.
        pool.refresh().await;
        assert_eq!(pool.members(), members);
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn console_session_connects_on_demand_by_replacing_an_idle_member() {
        let pool = TestPool::new(2, Duration::from_secs(600), 3).await;
        pool.refresh().await;
        let wanted = pool.unpooled_machine();

        let subscription = pool.connect(wanted).await.expect("on-demand connection");

        let members = pool.members();
        assert_eq!(members.len(), 2);
        assert!(members.contains(&(wanted, Origin::OnDemand)));
        drop(subscription);
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn pool_full_of_open_sessions_refuses_a_new_connection() {
        let pool = TestPool::new(1, Duration::from_secs(600), 2).await;
        pool.refresh().await;
        let (pooled, _) = pool.members()[0];
        let wanted = pool.unpooled_machine();

        let open_session = pool
            .connect(pooled)
            .await
            .expect("subscribe to pooled member");
        assert!(matches!(
            pool.connect(wanted).await,
            Err(GetConnectionError::PoolFull { machine_id }) if machine_id == wanted
        ));

        // Once the session ends, the pooled member can make room.
        drop(open_session);
        let subscription = pool.connect(wanted).await.expect("on-demand connection");
        assert_eq!(pool.members(), vec![(wanted, Origin::OnDemand)]);
        drop(subscription);
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn idle_on_demand_connection_closes_at_the_next_refresh() {
        for (scenario, idle_timeout, expect_closed) in [
            ("idle longer than the timeout", Duration::ZERO, true),
            (
                "idle shorter than the timeout",
                Duration::from_secs(3600),
                false,
            ),
        ] {
            let pool = TestPool::new(2, idle_timeout, 3).await;
            pool.refresh().await;
            let wanted = pool.unpooled_machine();
            drop(pool.connect(wanted).await.expect("on-demand connection"));

            pool.refresh().await;

            let members = pool.members();
            assert_eq!(members.len(), 2, "{scenario}");
            assert_eq!(
                members.contains(&(wanted, Origin::OnDemand)),
                !expect_closed,
                "{scenario}"
            );
            pool.shutdown().await;
        }
    }
}
