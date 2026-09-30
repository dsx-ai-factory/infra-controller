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

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use metrics_endpoint::{
    MetricsEndpointConfig, MetricsSetup, new_metrics_setup, run_metrics_endpoint_with_cancellation,
};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use ufm_mock::{InventoryConfig, UfmAuthToken, UfmMock};

use crate::config::{ControllerConfig, GatewayConfig};
use crate::health;
use crate::ownership::{Ownership, OwnershipHandle, OwnershipMap, RackStatusClient};
use crate::rms_proxy::{self, RmsProxy};
use crate::sources::{SourceList, SourceListCheck, SourceListClient, SourceSetChange};

/// Process exit code used when the controller's source set moved away from the one this gateway
/// started with. Kubernetes restarts the container, which re-reads the list.
pub const SOURCE_LIST_CHANGED_EXIT_CODE: i32 = 3;

/// Why [`run`] returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExitReason {
    /// The shutdown token was cancelled, typically by SIGTERM or SIGINT.
    Shutdown,
    /// The controller now publishes a different source set than the one used at startup.
    SourceListChanged(SourceSetChange),
}

impl ExitReason {
    /// Process exit status: 0 for shutdown, [`SOURCE_LIST_CHANGED_EXIT_CODE`] otherwise.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Shutdown => 0,
            Self::SourceListChanged(_) => SOURCE_LIST_CHANGED_EXIT_CODE,
        }
    }
}

/// UFM mock, RMS proxy and gateway-owned routes, bound to one controller source set.
///
/// The HTTP router is available immediately so that liveness probes succeed while the gateway
/// waits for the controller. Inventory reconciliation, rack ownership polling, RMS routing and
/// readiness start in [`Self::bootstrap`]; until then routed RMS RPCs answer `UNAVAILABLE`.
pub struct Gateway {
    config: GatewayConfig,
    ufm: UfmMock,
    metrics_setup: MetricsSetup,
    cancellation: CancellationToken,
    reconciliation: Option<JoinHandle<()>>,
    /// Stands in for the ownership map until bootstrap has the source list to build it from.
    ownership: OwnershipHandle,
    ownership_poll: Option<JoinHandle<()>>,
    rms: Arc<RmsProxy>,
}

impl Gateway {
    /// Creates the UFM mock, the RMS proxy and the metrics registry; nothing is served or polled
    /// yet. The CA file named by `sources.ca_cert_path` is read here.
    pub fn new(config: GatewayConfig, auth_token: &UfmAuthToken) -> eyre::Result<Self> {
        let metrics_setup = new_metrics_setup("carbide-mat-protocol-gateway", "carbide", false)?;
        // Not ready until `bootstrap` has a source list; the metrics endpoint shares the flag.
        metrics_setup.health_controller.set_ready(false);
        let ufm = UfmMock::new(&config.ufm, auth_token, &metrics_setup.meter)?;
        let ownership = OwnershipHandle::new();
        let rms = Arc::new(RmsProxy::new(
            Arc::new(ownership.clone()),
            &config.sources,
            &config.rms,
        )?);
        Ok(Self {
            config,
            ufm,
            metrics_setup,
            cancellation: CancellationToken::new(),
            reconciliation: None,
            ownership,
            ownership_poll: None,
            rms,
        })
    }

    /// UFM routes, probe routes and both RMS gRPC services on one router.
    pub fn router(&self) -> Router {
        self.ufm
            .router()
            .merge(health::router(
                self.metrics_setup.health_controller.clone(),
                self.ownership.clone(),
            ))
            .merge(rms_proxy::router(self.rms.clone()))
    }

    /// Waits for a usable source list, then binds the gateway to it: takes the first rack
    /// ownership snapshot of every source and keeps polling, points the RMS proxy at the
    /// instances, starts UFM reconciliation, and marks the gateway ready once every source has
    /// answered and no rack is claimed twice. Returns the list the gateway is now bound to.
    pub async fn bootstrap(&mut self, client: &SourceListClient) -> eyre::Result<SourceList> {
        let source_list = wait_for_source_list(client, &self.config.controller).await?;

        let ownership = OwnershipMap::new(&source_list, &self.config.ownership);
        let status_client = RackStatusClient::new(&self.config.sources, &self.config.ownership)?;
        ownership.poll_once(&status_client).await;
        for blocker in ownership.blockers() {
            tracing::warn!(%blocker, "Rack ownership is not ready after the initial poll");
        }
        self.rms.bind_sources(&source_list)?;
        self.ownership.bind(Arc::new(ownership.clone()));

        let static_sources = source_list.inventory_sources(&self.config.sources);
        tracing::info!(
            generation = %source_list.generation,
            sources = ?source_list.names(),
            pods = ?source_list.pods(),
            "Configuring UFM inventory from controller source list"
        );
        let inventory = InventoryConfig {
            static_sources,
            ..self.config.ufm.inventory.clone()
        };
        self.reconciliation = Some(self.ufm.start_reconciliation(
            inventory,
            None,
            self.cancellation.child_token(),
        ));
        let readiness = self.metrics_setup.health_controller.clone();
        readiness.set_ready(ownership.is_ready());
        self.ownership_poll =
            Some(ownership.spawn(status_client, readiness, self.cancellation.child_token()));
        Ok(source_list)
    }

    /// Starts the optional Prometheus endpoint configured by `metrics_address`.
    fn start_metrics_endpoint(&self) -> Option<JoinHandle<()>> {
        let address = self.config.metrics_address?;
        let metrics_config = MetricsEndpointConfig {
            address,
            registry: self.metrics_setup.registry.clone(),
            health_controller: Some(self.metrics_setup.health_controller.clone()),
            additional_prefix: None,
        };
        let cancellation = self.cancellation.child_token();
        Some(tokio::spawn(async move {
            if let Err(error) =
                run_metrics_endpoint_with_cancellation(&metrics_config, cancellation).await
            {
                tracing::error!(error = %error, "Gateway metrics endpoint stopped");
            }
        }))
    }

    /// Stops reconciliation, ownership polling and the metrics endpoint and waits for them to
    /// finish.
    pub async fn shutdown(self) -> eyre::Result<()> {
        self.cancellation.cancel();
        if let Some(reconciliation) = self.reconciliation {
            reconciliation.await?;
        }
        if let Some(ownership_poll) = self.ownership_poll {
            ownership_poll.await?;
        }
        drop(self.metrics_setup);
        Ok(())
    }
}

/// Polls the controller until it reports a ready, non-empty, valid source list.
///
/// Attempts are bounded by `controller.startup_attempts`; every failed attempt is logged so an
/// operator can tell a slow controller from a broken one.
pub async fn wait_for_source_list(
    client: &SourceListClient,
    controller: &ControllerConfig,
) -> eyre::Result<SourceList> {
    let attempts = controller.startup_attempts;
    let mut last_error = None;
    for attempt in 1..=attempts {
        match client.fetch().await {
            Ok(list) => match list.validate() {
                Ok(()) => return Ok(list),
                Err(error) => {
                    tracing::info!(
                        attempt,
                        attempts,
                        url = %client.url,
                        %error,
                        "Controller source list is not usable yet"
                    );
                    last_error = Some(eyre::Report::new(error));
                }
            },
            Err(error) => {
                tracing::warn!(
                    attempt,
                    attempts,
                    url = %client.url,
                    error = %error,
                    "Could not fetch controller source list"
                );
                last_error = Some(error);
            }
        }
        if attempt < attempts {
            tokio::time::sleep(controller.startup_retry_interval).await;
        }
    }
    Err(last_error
        .unwrap_or_else(|| eyre::eyre!("no source list attempts were made"))
        .wrap_err(format!(
            "controller source list at {} was not usable after {attempts} attempts",
            client.url
        )))
}

/// Periodically compares the controller's source list with `startup`.
///
/// Returns the first observed set change, or `None` when `cancellation` fires first. The
/// generation is only a hint: a controller restart republishes the unchanged set under a new
/// counter and is logged, not acted on, and a not-ready list from a restarting controller is
/// skipped. Fetch failures are logged and retried on the next tick; a temporarily unreachable
/// controller is not a reason to restart a working gateway.
pub async fn watch_source_list(
    client: &SourceListClient,
    startup: &SourceList,
    poll_interval: Duration,
    cancellation: &CancellationToken,
) -> Option<SourceSetChange> {
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick completes immediately; skip it so the first comparison happens one interval
    // after bootstrap.
    interval.tick().await;
    let mut known_generation = startup.generation;
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return None,
            _ = interval.tick() => {}
        }
        let fetched = tokio::select! {
            _ = cancellation.cancelled() => return None,
            result = client.fetch() => result,
        };
        let list = match fetched {
            Ok(list) => list,
            Err(error) => {
                tracing::warn!(url = %client.url, error = %error, "Could not refresh controller source list");
                continue;
            }
        };
        match SourceListCheck::compare(startup, &list) {
            SourceListCheck::Unchanged => {}
            SourceListCheck::NotReady { current } => {
                tracing::info!(
                    startup = %startup.generation,
                    %current,
                    "Controller is rediscovering its sources; keeping the current source set"
                );
            }
            SourceListCheck::GenerationMoved { current } => {
                if current != known_generation {
                    tracing::info!(
                        startup = %startup.generation,
                        %current,
                        "Controller source list generation moved with an unchanged source set"
                    );
                    known_generation = current;
                }
            }
            SourceListCheck::SetChanged(change) => {
                tracing::warn!(
                    startup = %change.startup,
                    current = %change.current,
                    added = ?change.added,
                    removed = ?change.removed,
                    "Controller source set changed"
                );
                return Some(change);
            }
        }
    }
}

/// Outcome of one phase of [`run`], separated from the phase's `select!` so that the gateway can
/// be shut down after the competing futures have been dropped.
enum Phase<T> {
    Completed(T),
    Shutdown,
    ServerStopped(Result<eyre::Result<()>, JoinError>),
}

/// Runs the gateway until shutdown or a source set change.
pub async fn run(
    config: GatewayConfig,
    auth_token: &UfmAuthToken,
    shutdown: CancellationToken,
) -> eyre::Result<ExitReason> {
    let controller = SourceListClient::new(
        config.controller.sources_url.clone(),
        config.controller.request_timeout,
    )?;
    let mut gateway = Gateway::new(config, auth_token)?;
    let metrics_task = gateway.start_metrics_endpoint();

    tracing::info!(
        address = %gateway.config.listen_address,
        tls = gateway.config.tls.is_some(),
        "Starting machine-a-tron protocol gateway"
    );
    let mut server = tokio::spawn(ufm_mock::serve(
        gateway.config.listen_address,
        gateway.config.tls.clone(),
        gateway.router(),
        shutdown.child_token(),
    ));

    let bootstrap = tokio::select! {
        result = gateway.bootstrap(&controller) => Phase::Completed(result),
        _ = shutdown.cancelled() => Phase::Shutdown,
        result = &mut server => Phase::ServerStopped(result),
    };
    let source_list = match bootstrap {
        Phase::Completed(Ok(source_list)) => source_list,
        Phase::Completed(Err(error)) => {
            finish(gateway, Some(server), metrics_task, &shutdown).await?;
            return Err(error);
        }
        Phase::Shutdown => {
            finish(gateway, Some(server), metrics_task, &shutdown).await?;
            return Ok(ExitReason::Shutdown);
        }
        Phase::ServerStopped(result) => {
            // Cancelling `shutdown` also stops the listener, and `select!` may report the
            // listener before the shutdown branch; that is a clean shutdown, not a failure.
            let requested = shutdown.is_cancelled();
            finish(gateway, None, metrics_task, &shutdown).await?;
            result??;
            if requested {
                return Ok(ExitReason::Shutdown);
            }
            eyre::bail!("gateway listener stopped before bootstrap completed");
        }
    };

    let poll_interval = gateway.config.controller.poll_interval;
    let watch = tokio::select! {
        change = watch_source_list(&controller, &source_list, poll_interval, &shutdown) => {
            Phase::Completed(change)
        }
        result = &mut server => Phase::ServerStopped(result),
    };
    match watch {
        Phase::Completed(Some(change)) => {
            finish(gateway, Some(server), metrics_task, &shutdown).await?;
            Ok(ExitReason::SourceListChanged(change))
        }
        Phase::Completed(None) | Phase::Shutdown => {
            finish(gateway, Some(server), metrics_task, &shutdown).await?;
            Ok(ExitReason::Shutdown)
        }
        Phase::ServerStopped(result) => {
            // As above: the watch returns `None` and the listener stops on the same cancellation,
            // and either may be reported first.
            let requested = shutdown.is_cancelled();
            finish(gateway, None, metrics_task, &shutdown).await?;
            result??;
            if requested {
                return Ok(ExitReason::Shutdown);
            }
            eyre::bail!("gateway listener stopped unexpectedly");
        }
    }
}

async fn finish(
    gateway: Gateway,
    server: Option<JoinHandle<eyre::Result<()>>>,
    metrics_task: Option<JoinHandle<()>>,
    shutdown: &CancellationToken,
) -> eyre::Result<()> {
    shutdown.cancel();
    if let Some(server) = server {
        server.await??;
    }
    gateway.shutdown().await?;
    if let Some(metrics_task) = metrics_task {
        metrics_task.await?;
    }
    Ok(())
}
