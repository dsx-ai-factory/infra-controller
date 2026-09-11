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

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use futures_util::future::join_all;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Meter, ObservableGauge};
use rpc::forge;
use rpc::forge_api_client::ForgeApiClient;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Receiver;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::bmc::client::{BmcConnectionSubscription, ClientHandle};
use crate::bmc::client_pool::GetConnectionError::InvalidMachineId;
use crate::bmc::connection::State;
use crate::bmc::{client, connection};
use crate::config::Config;
use crate::shutdown_handle::{ReadyHandle, ShutdownHandle};
use crate::ssh_server::ServerMetrics;

/// Spawn a background task that connects to all BMC's in the environment, reconnecting if they fail.
pub(crate) fn spawn(
    config: Arc<Config>,
    forge_api_client: ForgeApiClient,
    meter: &Meter,
) -> Handle {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (ready_tx, ready_rx) = oneshot::channel();
    let members: Arc<RwLock<HashMap<MachineId, ClientHandle>>> = Default::default();
    let join_handle = tokio::spawn(
        BmcPool {
            members: members.clone(),
            shutdown_rx,
            config,
            forge_api_client,
            metrics: Arc::new(BmcPoolMetrics::new(meter, members.clone())),
        }
        .run_loop(ready_tx),
    );

    Handle {
        shutdown_tx,
        connection_store: BmcConnectionStore(members),
        ready_rx: Some(ready_rx),
        join_handle,
    }
}

/// A owned handle to the entire BMC client pool background task. The pool will shut down when this is
/// dropped.
pub(crate) struct Handle {
    connection_store: BmcConnectionStore,
    shutdown_tx: oneshot::Sender<()>,
    ready_rx: Option<oneshot::Receiver<()>>,
    join_handle: tokio::task::JoinHandle<()>,
}

impl Handle {
    pub(crate) fn connection_store(&self) -> BmcConnectionStore {
        self.connection_store.clone()
    }
}

impl ShutdownHandle<()> for Handle {
    fn into_parts(self) -> (oneshot::Sender<()>, JoinHandle<()>) {
        (self.shutdown_tx, self.join_handle)
    }
}

impl ReadyHandle for Handle {
    fn take_ready_rx(&mut self) -> Option<Receiver<()>> {
        self.ready_rx.take()
    }
}

/// An Arc reference to the available BMC connections in this pool
#[derive(Clone)]
pub(crate) struct BmcConnectionStore(Arc<RwLock<HashMap<MachineId, ClientHandle>>>);

impl BmcConnectionStore {
    pub(crate) async fn get_connection(
        &self,
        machine_or_instance_id: &str,
        config: &Config,
        forge_api_client: &ForgeApiClient,
        metrics: Arc<ServerMetrics>,
    ) -> Result<BmcConnectionSubscription, GetConnectionError> {
        if let Ok(machine_id) = MachineId::from_str(machine_or_instance_id) {
            self.0
                .read()
                .expect("lock poisoned")
                .get(&machine_id)
                .map(|session_handle| session_handle.subscribe(metrics))
                .ok_or_else(|| GetConnectionError::InvalidMachineId {
                    machine_or_instance_id: machine_or_instance_id.to_string(),
                })
        } else if let Ok(instance_id) = InstanceId::from_str(machine_or_instance_id) {
            let machine_id_candidate = if let Some(machine_id) =
                config.override_bmcs.iter().flatten().find_map(|bmc| {
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
                }) {
                machine_id
            } else {
                forge_api_client
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
                    .ok_or_else(|| GetConnectionError::InstanceMissingMachineId { instance_id })?
            };

            self.0
                .read()
                .expect("lock poisoned")
                .get(&machine_id_candidate.into())
                .map(|session_handle| session_handle.subscribe(metrics))
                .ok_or_else(|| GetConnectionError::NoMachineWithInstanceId { instance_id })
        } else {
            Err(InvalidMachineId {
                machine_or_instance_id: machine_or_instance_id.to_owned(),
            })
        }
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
    #[error("no machine with instance_id {instance_id}")]
    NoMachineWithInstanceId { instance_id: InstanceId },
}

/// A BmcPool runs in a background Task and maintains a single BmcSession handle to each
/// BMC
struct BmcPool {
    members: Arc<RwLock<HashMap<MachineId, ClientHandle>>>,
    shutdown_rx: oneshot::Receiver<()>,
    config: Arc<Config>,
    forge_api_client: ForgeApiClient,
    metrics: Arc<BmcPoolMetrics>,
}

pub(super) struct BmcPoolMetrics {
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
    fn new(meter: &Meter, members: Arc<RwLock<HashMap<MachineId, ClientHandle>>>) -> Self {
        Self {
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
                            .fold(0, |acc, conn| {
                                if conn.connection_state.load() == State::ConnectionError { acc + 1 } else { acc }
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
                            .fold(0, |acc, conn| {
                                if conn.connection_state.load() == State::Connected { acc + 1 } else { acc }
                            });
                        observer.observe(error_count, &[]);
                    }
                })
                .build(),
            _bmc_status: meter.u64_observable_gauge("ssh_console_bmc_status")
                .with_description("Current status of the session to the bmc, see value label")
                .with_callback({
                    move |observer| {
                        members.read().expect("lock poisoned").iter().for_each(|(machine_id, handle)| {
                            let state = handle.connection_state.load();
                            observer.observe(state as _, &[
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
    pub(super) fn for_test() -> Self {
        Self::new(
            &opentelemetry::global::meter("ssh-console-ipmi-test"),
            Arc::default(),
        )
    }
}

impl BmcPool {
    /// Run a loop which refreshes the list of BMC's from the API and ensures we have a running
    /// connection to each one.
    async fn run_loop(mut self, ready_tx: oneshot::Sender<()>) {
        let mut api_refresh = tokio::time::interval(self.config.api_poll_interval);
        // Don't try to catch up if for some reason api refresh takes forever (ie. if the connection
        // is down and we have to retry a long time.)
        api_refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut ready_tx = Some(ready_tx);

        loop {
            tokio::select! {
                _ = &mut self.shutdown_rx => {
                    tracing::info!("shutting down BmcPool");
                    break;
                }
                _ = api_refresh.tick() => {
                    if let Err(error) = self.refresh_bmcs().await {
                        tracing::error!(%error, "error refreshing BMC list from API");
                    }
                    // Inform callers that we're ready once the first API refresh happens.
                    ready_tx.take().map(|ch| ch.send(()).ok());
                }
            }
        }

        // Shutdown each BMC connection
        let members = self
            .members
            .write()
            .expect("lock poisoned")
            .drain()
            .map(|(_machine_id, handle)| handle)
            .collect::<Vec<_>>();
        join_all(members.into_iter().map(|handle| handle.shutdown_and_wait())).await;
    }

    async fn refresh_bmcs(&mut self) -> Result<(), RefreshBmcsError> {
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
                    self.forge_api_client
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
            let mut guard = self.members.write().expect("lock poisoned");

            // Remove any machines that are no longer monitored
            let to_remove = guard
                .keys()
                .filter(|&machine_id| !machine_ids.contains(machine_id))
                .copied()
                .collect::<Vec<_>>();
            for machine_id in to_remove {
                tracing::info!(%machine_id, "removing machine from console logging, no longer found in carbide");
                guard.remove(&machine_id);
            }

            // Add any machines that need to be monitored
            machine_ids
                .iter()
                .filter(|id| !guard.contains_key(id))
                .copied()
                .collect::<Vec<_>>()
        };

        // For each one we want to add, get the connection details. Skip any machines which fail
        // here.
        let all_connection_details = join_all(to_add.into_iter().map(|machine_id| {
            let config = self.config.clone();
            let forge_api_client = self.forge_api_client.clone();
            async move {
                match connection::lookup(
                    &machine_id.to_string(),
                    &config,
                    &forge_api_client,
                )
                    .await
                {
                    Ok(connection_details) => Some((machine_id, connection_details)),
                    Err(error) => {
                        tracing::error!(%machine_id, %error, "error looking up connection details, excluding from bmc list");
                        None
                    }
                }
            }
        })).await.into_iter().flatten().collect::<Vec<_>>();

        {
            // Now add each of these to the pool
            let mut guard = self.members.write().expect("lock poisoned");
            for (machine_id, connection_details) in all_connection_details {
                if guard.contains_key(&machine_id) {
                    continue;
                }
                tracing::info!(%machine_id, "begin connection to machine");
                let bmc_session_handle = client::spawn(
                    connection_details,
                    self.config.clone(),
                    self.metrics.clone(),
                );
                guard.insert(machine_id, bmc_session_handle);
            }

            self.metrics.total_machines.record(guard.len() as _, &[]);
        }

        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
enum RefreshBmcsError {
    #[error("error fetching machine ids: {tonic_status}")]
    FetchingMachineIdsFailure { tonic_status: tonic::Status },
}
