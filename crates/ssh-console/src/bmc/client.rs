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
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use futures_util::FutureExt;
use opentelemetry::KeyValue;
use russh::ChannelMsg;
use tokio::net::TcpStream;
use tokio::sync::{MutexGuard, broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::bmc::client_pool::BmcPoolMetrics;
use crate::bmc::connection::{self, AtomicConnectionState, ConnectionDetails};
use crate::bmc::message_proxy::{
    ConnectionChangeMessage, ExecReply, ToBmcMessage, ToFrontendMessage,
};
use crate::config::Config;
use crate::shutdown_handle::ShutdownHandle;
use crate::ssh_server::ServerMetrics;
use crate::{console_logger, fork_cancel_token};

/// Spawn a connection to the given BMC in the background, returning a handle. Connections will
/// be retried indefinitely, with exponential backoff, until a shutdown is signaled (ie. by dropping
/// the ClientHandle.)
pub(super) fn spawn(
    connection_details: ConnectionDetails,
    config: Arc<Config>,
    metrics: Arc<BmcPoolMetrics>,
    cancel_token: CancellationToken,
) -> ClientHandle {
    let (cancel_token, drop_guard) = fork_cancel_token(cancel_token);

    // Channel frontends can use to send messages to the BMC
    let (to_bmc_msg_tx, to_bmc_msg_rx) = mpsc::channel::<ToBmcMessage>(1);
    // Channel that broadcasts messages to any subscribed frontends
    let (broadcast_to_frontend_tx, broadcast_to_frontend_rx) =
        broadcast::channel::<ToFrontendMessage>(4096);

    // Always consume messages from the frontend broadcast channel, even if there are no frontends.
    dev_null(broadcast_to_frontend_rx);

    let connection_state = Arc::new(AtomicConnectionState::default());
    let machine_id = connection_details.machine_id();
    let kind = connection_details.kind();
    let (logger_handle, console_log_client) = if config.console_logging_enabled {
        let (handle, client) = console_logger::spawn(
            machine_id,
            connection_details.addr(),
            broadcast_to_frontend_tx.subscribe(),
            config.clone(),
            cancel_token.clone(),
        );
        (Some(handle), Some(client))
    } else {
        (None, None)
    };

    let metrics_attrs = vec![KeyValue::new("machine_id", machine_id.to_string())];
    metrics.bmc_recovery_attempts.record(0, &metrics_attrs);
    metrics.bmc_rx_errors_total.add(0, &metrics_attrs);
    metrics.bmc_tx_errors_total.add(0, &metrics_attrs);
    metrics.bmc_bytes_received_total.add(0, &metrics_attrs);

    let bmc_client = BmcClient {
        connection_details,
        config,
        connection_state: connection_state.clone(),
        broadcast_to_frontend_tx: broadcast_to_frontend_tx.clone(),
        cancel_token,
        to_bmc_msg_rx,
        metrics,
        logger_handle,
    };

    let join_handle = tokio::spawn(bmc_client.run());

    ClientHandle {
        to_bmc_msg_tx,
        broadcast_to_frontend_tx,
        machine_id,
        drop_guard,
        join_handle,
        connection_state,
        kind,
        console_log_client,
    }
}

struct BmcClient {
    connection_details: ConnectionDetails,
    config: Arc<Config>,
    connection_state: Arc<AtomicConnectionState>,
    cancel_token: CancellationToken,
    broadcast_to_frontend_tx: broadcast::Sender<ToFrontendMessage>,
    to_bmc_msg_rx: mpsc::Receiver<ToBmcMessage>,
    metrics: Arc<BmcPoolMetrics>,
    logger_handle: Option<console_logger::ConsoleLoggerHandle>,
}

impl BmcClient {
    async fn run(mut self) {
        let machine_id = self.connection_details.machine_id();
        let metrics_attrs = vec![KeyValue::new("machine_id", machine_id.to_string())];

        let logger_handle = self.logger_handle.take();

        // Keep track of when we were last disconnected, for relaying status
        let last_disconnect_time: Arc<RwLock<Option<DateTime<Utc>>>> = Default::default();

        // Spawn a message relay for communicating status to the user if the BMC is
        // disconnected.
        let bmc_msg_tx_placeholder = BmcMessageTxPlaceholder::default();
        let bmc_message_relay = relay_input_to_bmc(
            self.broadcast_to_frontend_tx.clone(),
            self.connection_state.clone(),
            self.to_bmc_msg_rx,
            bmc_msg_tx_placeholder.clone(),
            last_disconnect_time.clone(),
            self.cancel_token.clone(),
        );

        // Keep track of the instant we do the next retry, and not the duration to wait: This helps
        // it so that if we wait a long time for a host to be up, we don't wait longer than we need
        // to. (e.g. if retry time is 1 minute, and the machine takes 5 minutes to be up, we don't
        // want to wait 6 minutes.)
        let mut next_retry = Instant::now();
        let mut was_disconnected = false;

        // Connect and reconnect, in a loop, until the client is shut down.
        let mut retries = 0;
        let mut previous_connection_close_was_sol_recovery = false;
        'retry: loop {
            if self.cancel_token.is_cancelled() {
                break 'retry;
            }
            // Every retry after the first time, emit a disconnected message
            if was_disconnected {
                self.broadcast_to_frontend_tx
                    .send(ToFrontendMessage::ConnectionChanged(
                        ConnectionChangeMessage::Disconnected,
                    ))
                    .ok();
                *last_disconnect_time.write().expect("lock poisoned") = Some(Utc::now());
            } else {
                was_disconnected = true;
            }

            self.metrics
                .bmc_recovery_attempts
                .record(retries, metrics_attrs.as_slice());
            if retries == 0 {
                self.connection_state.store(connection::State::Connecting);
            } else {
                self.connection_state
                    .store(connection::State::ConnectionError);
            }
            retries += 1;

            // Wait for the host to be up, so we avoid long backoff times when the BMC is just down.
            // This way, the moment it comes back up, we can start trying again straight away.
            if let Err(error) = wait_until_host_is_up(
                self.connection_details.addr(),
                self.connection_details.kind(),
                self.connection_details.machine_id(),
                self.cancel_token.clone(),
            )
            .await
            {
                tracing::error!(%machine_id, %error, "error checking BMC port");
                continue 'retry;
            }

            // Only sleep for the remaining retry time after we did the TCP halfopen check.
            let sleep_duration = {
                let now = Instant::now();
                if next_retry > now {
                    next_retry - now
                } else {
                    Duration::ZERO
                }
            };

            tokio::time::sleep(sleep_duration).await;

            // Subsequent retries should sleep for RETRY_BASE_DURATION and double from there
            // until we successfully connect.
            next_retry = Instant::now() + next_retry_backoff(&self.config, sleep_duration);

            let try_start_time = Instant::now();

            // Spawn a single connection to this BMC.
            let bmc_connection_handle = match connection::spawn(
                self.connection_details.clone(),
                self.broadcast_to_frontend_tx.clone(),
                self.cancel_token.clone(),
                self.metrics.clone(),
                self.config.clone(),
            )
            .await
            {
                Ok(handle) => handle,
                Err(error) => {
                    previous_connection_close_was_sol_recovery = false;
                    tracing::error!(
                        %error,
                        %machine_id,
                        retry_delay_seconds = next_retry
                            .checked_duration_since(Instant::now())
                            .unwrap_or_default()
                            .as_secs(),
                        "error spawning BMC connection, will retry"
                    );
                    continue 'retry;
                }
            };

            // Successfully ready, give the BMC channel to the message relay and set the state
            // to Connected. (if ready_rx is not ok, then the tx must have been dropped, and
            // we'll report errors and retry below.)
            bmc_msg_tx_placeholder
                .replace(Some(bmc_connection_handle.to_bmc_msg_tx))
                .await;
            self.connection_state.store(connection::State::Connected);
            self.broadcast_to_frontend_tx
                .send(ToFrontendMessage::ConnectionChanged(
                    ConnectionChangeMessage::Connected {
                        last_disconnect: *last_disconnect_time.read().expect("lock poisoned"),
                    },
                ))
                .ok();

            let connection_result = async move {
                bmc_connection_handle
                    .join_handle
                    .await
                    .expect("task panicked")
                    .map_err(Arc::new)
            }
            .shared();

            tokio::select! {
                // Our cancel token propagates to bmc_connection_handle, so wait for it to finish
                _ = self.cancel_token.cancelled() => {
                    tracing::info!(%machine_id, "shutting down BMC connection");
                    if let Err(error) = connection_result.await {
                        tracing::error!(%machine_id, error = %error.as_ref(), "BMC connection failed while shutting down");
                    };
                    break 'retry;
                }

                // The connection should go forever, so if it doesn't, retry.
                res = connection_result.clone() => {
                    if self.cancel_token.is_cancelled() {
                        break 'retry;
                    }
                    let connection_time = try_start_time.elapsed();
                    let recovered_conflicting_sol_session = res
                        .as_ref()
                        .is_err_and(|error| error.retry_immediately());
                    if should_reset_retry_backoff(
                        connection_time,
                        self.config.successful_connection_minimum_duration,
                        &res,
                        previous_connection_close_was_sol_recovery,
                    ) {
                        if recovered_conflicting_sol_session {
                            tracing::debug!(%machine_id, "retrying immediately after IPMI SOL session recovery");
                        } else {
                            tracing::debug!(
                                %machine_id,
                                connection_duration_seconds = connection_time.as_secs_f64(),
                                "last connection succeeded long enough; resetting backoff"
                            );
                        }
                        next_retry = Instant::now();
                    }
                    previous_connection_close_was_sol_recovery =
                        recovered_conflicting_sol_session;
                    let error_string = res.err().map(|e| format!("{:?}", e.as_ref())).unwrap_or("<none>".to_string());
                    tracing::warn!(
                        %machine_id,
                        error = error_string,
                        connection_duration_seconds = connection_time.as_secs_f64(),
                        retry_delay_seconds = next_retry
                            .checked_duration_since(Instant::now())
                            .unwrap_or_default()
                            .as_secs(),
                        "connection to BMC closed, will retry",
                    );
                }
            }
        }

        // Clean up: Shut down message relay and logger
        bmc_message_relay.shutdown_and_wait().await;
        if let Some(logger_handle) = logger_handle {
            logger_handle.shutdown_and_wait().await;
        }
    }
}

/// Wait for an address to be "up". For SSH consoles, do a a TCP half-open connection request. For
/// IPMI machines, just ping it. For each probe, wait for 2 seconds for a reply, and retry every 5
/// seconds until we get a connection.
async fn wait_until_host_is_up(
    addr: SocketAddr,
    kind: connection::Kind,
    machine_id: MachineId,
    cancel_token: CancellationToken,
) -> io::Result<()> {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut do_log = true;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                match kind {
                    connection::Kind::Ssh => {
                        if let Some(Ok(Ok(_))) = cancel_token.run_until_cancelled(tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(addr))).await {
                            break Ok(());
                        }
                    }
                    connection::Kind::Ipmi => {
                        if let Some(true) = cancel_token.run_until_cancelled(check_ipmi_reachable(addr, Duration::from_secs(2))).await {
                            break Ok(());
                        }
                    }
                }
            }
            _ = cancel_token.cancelled() => {
                break Ok(());
            }
        }

        if do_log {
            do_log = false;
            tracing::info!(
                %machine_id,
                bmc_address = %addr,
                "BMC is not listening; waiting for the port to open before connecting"
            )
        }
    }
}

/// Spawn a task which will take messages from to_bmc_msg_rx and either relay them to a
/// BMC connection, *or* reply to the user saying the BMC is disconnected, depending on
/// whether the BMC connection is healthy.
///
/// - `bmc_msg_tx_placeholder`: A shareable placeholder for a channel to send messages to
///   the BMC, once the connection is ready.
fn relay_input_to_bmc(
    broadcast_to_frontend_tx: broadcast::Sender<ToFrontendMessage>,
    connection_state: Arc<AtomicConnectionState>,
    mut to_bmc_msg_rx: mpsc::Receiver<ToBmcMessage>,
    bmc_msg_tx_placeholder: BmcMessageTxPlaceholder,
    last_disconnect_time: Arc<RwLock<Option<DateTime<Utc>>>>,
    cancel_token: CancellationToken,
) -> MessageRelayHandle {
    let (cancel_token, drop_guard) = fork_cancel_token(cancel_token);
    let join_handle = tokio::spawn({
        async move {
            while let Some(msg) = cancel_token
                .run_until_cancelled(to_bmc_msg_rx.recv())
                .await
                .flatten()
            {
                let bmc_tx_guard = if let connection::State::Connected = connection_state.load() {
                    Some(bmc_msg_tx_placeholder.lock().await)
                } else {
                    None
                };

                // If we're connected, relay the message
                if let Some(tx) = bmc_tx_guard.as_ref().and_then(|guard| guard.as_ref()) {
                    tx.send(msg).await.ok();
                } else {
                    // Otherwise, when the user types a newline, inform them the BMC
                    // is not connected
                    let inform_disconnected = match msg {
                        ToBmcMessage::ChannelMsg(ChannelMsg::Data { data }) => {
                            data.contains(&b'\r') || data.contains(&b'\n')
                        }
                        ToBmcMessage::EchoConnectionMessage { reply_tx: _ } => true,
                        ToBmcMessage::Exec { reply_tx, .. } => {
                            reply_tx
                                .send(ExecReply {
                                    output: b"BMC console not connected\r\n".to_vec(),
                                    exit_status: 1,
                                })
                                .ok();
                            false
                        }
                        _ => false,
                    };

                    if inform_disconnected {
                        broadcast_to_frontend_tx
                            .send(ToFrontendMessage::InformDisconnectedSince(
                                *last_disconnect_time.read().expect("lock poisoned"),
                            ))
                            .ok();
                    }
                }
            }
        }
    });

    MessageRelayHandle {
        drop_guard,
        join_handle,
    }
}

struct MessageRelayHandle {
    drop_guard: DropGuard,
    join_handle: JoinHandle<()>,
}

#[derive(Default, Clone)]
struct BmcMessageTxPlaceholder(Arc<tokio::sync::Mutex<Option<mpsc::Sender<ToBmcMessage>>>>);

impl BmcMessageTxPlaceholder {
    #[inline]
    async fn replace(&self, value: Option<mpsc::Sender<ToBmcMessage>>) {
        *self.lock().await = value;
    }

    #[inline]
    async fn lock(&self) -> MutexGuard<'_, Option<mpsc::Sender<ToBmcMessage>>> {
        self.0.lock().await
    }
}

impl ShutdownHandle<()> for MessageRelayHandle {
    fn into_parts(self) -> (DropGuard, JoinHandle<()>) {
        (self.drop_guard, self.join_handle)
    }
}

/// Consume all the messages of a broadcast::Receiver, doing nothing with them, until the channel is
/// closed. This is a quick and dirty way to prevent a BMC's to_frontend_tx channel from returning
/// failures due to nobody listening. (Listeners may come and go, as logging is optional.)
fn dev_null<T: Clone + Send + 'static>(mut rx: broadcast::Receiver<T>) {
    tokio::spawn(async move {
        loop {
            if rx.recv().await.is_err() {
                return;
            };
        }
    });
}

fn should_reset_retry_backoff(
    connection_time: Duration,
    successful_connection_minimum_duration: Duration,
    connection_result: &Result<(), Arc<connection::SpawnError>>,
    previous_connection_close_was_sol_recovery: bool,
) -> bool {
    let recovered_conflicting_sol_session = connection_result
        .as_ref()
        .is_err_and(|error| error.retry_immediately());

    if recovered_conflicting_sol_session {
        // Retry one activation immediately after recovery, then use normal backoff if the conflict
        // persists so competing SOL clients cannot cause a tight deactivate/activate loop.
        !previous_connection_close_was_sol_recovery
    } else {
        connection_time > successful_connection_minimum_duration
    }
}

/// Calculate the next exponential backoff duration for retrying connections to a console
fn next_retry_backoff(config: &Config, prev: Duration) -> Duration {
    let duration = if prev == Duration::ZERO {
        config.reconnect_interval_base
    } else {
        // Sleep a random interval between prev and prev * 3
        let upper = (prev.as_secs_f64() * 3.0).min(config.reconnect_interval_max.as_secs_f64());
        Duration::from_secs_f64(rand::random_range(prev.as_secs_f64()..upper))
    };
    tracing::debug!(
        previous_backoff_milliseconds = prev.as_millis(),
        next_backoff_milliseconds = duration.as_millis(),
        "increasing connection retry backoff"
    );
    duration
}

pub(super) struct ClientHandle {
    machine_id: MachineId,
    kind: connection::Kind,
    /// Writer to send messages (including data) to BMC
    to_bmc_msg_tx: mpsc::Sender<ToBmcMessage>,
    // Hold a copy of the tx for broadcasting to frontends, so that we can subscribe to it multiple
    // times.
    broadcast_to_frontend_tx: broadcast::Sender<ToFrontendMessage>,
    drop_guard: DropGuard,
    join_handle: JoinHandle<()>,
    // Read by the pool's observable-gauge callbacks.
    pub(super) connection_state: Arc<AtomicConnectionState>,
    console_log_client: Option<console_logger::ConsoleLogClient>,
}

impl ShutdownHandle<()> for ClientHandle {
    fn into_parts(self) -> (DropGuard, JoinHandle<()>) {
        (self.drop_guard, self.join_handle)
    }
}

impl ClientHandle {
    pub(super) fn console_log_client(&self) -> Option<console_logger::ConsoleLogClient> {
        self.console_log_client.clone()
    }

    pub(super) fn subscribe(&self, metrics: Arc<ServerMetrics>) -> BmcConnectionSubscription {
        tracing::debug!("new bmc subscription");
        metrics.total_clients.add(1, &[]);
        metrics.bmc_clients.add(
            1,
            &[KeyValue::new("machine_id", self.machine_id.to_string())],
        );

        BmcConnectionSubscription {
            machine_id: self.machine_id,
            to_frontend_msg_weak_tx: self.broadcast_to_frontend_tx.downgrade(),
            to_bmc_msg_tx: self.to_bmc_msg_tx.clone(),
            metrics,
            kind: self.kind,
        }
    }
}

/// An individual "subscription" to a BMC connection, expected to be used by a frontend. Metrics
/// are affected when one is created or dropped.
pub(crate) struct BmcConnectionSubscription {
    pub(crate) machine_id: MachineId,
    pub(crate) to_frontend_msg_weak_tx: broadcast::WeakSender<ToFrontendMessage>,
    pub(crate) to_bmc_msg_tx: mpsc::Sender<ToBmcMessage>,
    pub(crate) kind: connection::Kind,
    // Not pub, to make sure we go through ClientHandle::subscribe() to build, so we get the
    // right metrics
    metrics: Arc<ServerMetrics>,
}

impl Drop for BmcConnectionSubscription {
    // Decrement metrics when dropping
    fn drop(&mut self) {
        tracing::debug!("dropping bmc subscription");
        self.metrics.total_clients.add(-1, &[]);
        self.metrics.bmc_clients.add(
            -1,
            &[KeyValue::new("machine_id", self.machine_id.to_string())],
        );
    }
}

/// Send an ASF Presence Ping (RMCP) to check if an IPMI endpoint is reachable.
async fn check_ipmi_reachable(addr: SocketAddr, timeout: Duration) -> bool {
    // Reference: IPMI v2.0 spec, section 13.2.3
    const ASF_PRESENCE_PING: [u8; 12] = [
        0x06, 0x00, 0xff, 0x06, 0x00, 0x00, 0x11, 0xbe, 0x80, 0x00, 0x00, 0x00,
    ];

    let Ok(socket) = tokio::net::UdpSocket::bind("0.0.0.0:0").await else {
        tracing::debug!(%addr, "failed to bind UDP socket for IPMI reachability check");
        return false;
    };

    if let Err(e) = socket.send_to(&ASF_PRESENCE_PING, addr).await {
        tracing::debug!(%addr, error = %e, "failed to send ASF Presence Ping");
        return false;
    }

    let mut recv_buf = [0u8; 32];
    match tokio::time::timeout(timeout, socket.recv_from(&mut recv_buf)).await {
        Ok(Ok((len, _))) => {
            tracing::debug!(%addr, response_len = len, "IPMI endpoint responded to ASF Presence Ping");
            true
        }
        Ok(Err(e)) => {
            tracing::debug!(%addr, error = %e, "error receiving ASF Presence Pong");
            false
        }
        Err(_) => {
            tracing::debug!(%addr, timeout_secs = ?timeout, "ASF Presence Ping timed out");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use super::*;
    use crate::bmc::connection_impl::ipmi;

    #[test]
    fn retry_backoff_resets_only_after_healthy_connection_or_successful_sol_recovery() {
        let minimum_healthy_duration = Duration::from_secs(60);
        let cases = [
            (
                "short successful connection",
                Duration::from_secs(1),
                Ok(()),
                false,
                false,
            ),
            (
                "exact minimum connection duration",
                minimum_healthy_duration,
                Ok(()),
                false,
                false,
            ),
            (
                "long-lived connection",
                minimum_healthy_duration + Duration::from_secs(1),
                Ok(()),
                false,
                true,
            ),
            (
                "ordinary IPMI failure",
                Duration::from_secs(1),
                Err(Arc::new(connection::SpawnError::Ipmi(
                    ipmi::SpawnError::IpmitoolUnexpectedExit {
                        exit_status: failed_exit_status(),
                        output: "authentication failed".to_string(),
                    },
                ))),
                false,
                false,
            ),
            (
                "successful conflicting SOL session recovery",
                Duration::from_secs(1),
                Err(Arc::new(connection::SpawnError::Ipmi(
                    ipmi::SpawnError::ConflictingSolSessionDeactivated {
                        exit_status: failed_exit_status(),
                        output: "SOL payload already active on another session".to_string(),
                    },
                ))),
                false,
                true,
            ),
            (
                "repeated successful conflicting SOL session recovery",
                Duration::from_secs(1),
                Err(Arc::new(connection::SpawnError::Ipmi(
                    ipmi::SpawnError::ConflictingSolSessionDeactivated {
                        exit_status: failed_exit_status(),
                        output: "SOL payload already active on another session".to_string(),
                    },
                ))),
                true,
                false,
            ),
            (
                "failed conflicting SOL session recovery",
                Duration::from_secs(1),
                Err(Arc::new(connection::SpawnError::Ipmi(
                    ipmi::SpawnError::ConflictingSolSessionDeactivationFailed {
                        exit_status: failed_exit_status(),
                        output: "SOL payload already active on another session".to_string(),
                        error: ipmi::SolDeactivateError::Failure {
                            exit_status: failed_exit_status(),
                            output: "deactivation failed".to_string(),
                        },
                    },
                ))),
                false,
                false,
            ),
        ];

        for (scenario, connection_time, result, previous_was_recovery, expected) in cases {
            assert_eq!(
                should_reset_retry_backoff(
                    connection_time,
                    minimum_healthy_duration,
                    &result,
                    previous_was_recovery,
                ),
                expected,
                "{scenario}",
            );
        }
    }

    fn failed_exit_status() -> std::process::ExitStatus {
        std::process::ExitStatus::from_raw(256)
    }
}
