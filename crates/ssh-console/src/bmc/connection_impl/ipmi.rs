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

use std::collections::VecDeque;
use std::fmt::Debug;
use std::future::Future;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use nix::errno::Errno;
use nix::pty::OpenptyResult;
use nix::unistd;
use opentelemetry::KeyValue;
use russh::ChannelMsg;
use tokio::io::unix::AsyncFd;
use tokio::process::Child;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::bmc::client_pool::BmcPoolMetrics;
use crate::bmc::connection_impl::echo_connected_message;
use crate::bmc::message_proxy::{ExecReply, ToBmcMessage, ToFrontendMessage};
use crate::bmc::pending_output_line::PendingOutputLine;
use crate::bmc::vendor::IPMITOOL_ESCAPE_SEQUENCE;
use crate::config::Config;
use crate::io_util::{
    self, PtyAllocError, set_controlling_terminal_on_exec, write_data_to_async_fd,
};
use crate::{POWER_RESET_COMMAND, fork_cancel_token};

const IPMITOOL_PASSWORD_ENV_VAR: &str = "IPMITOOL_PASSWORD";
const SOL_PAYLOAD_ALREADY_ACTIVE: &str = "SOL payload already active on another session";
const SOL_SESSION_OPERATIONAL: &[u8] = b"SOL Session operational";
const MAX_CAPTURED_IPMITOOL_OUTPUT_SIZE: usize = 4096;
const SOL_DEACTIVATE_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawn ipmitool in the background to connect to the given BMC specified by `connection_details`,
/// and proxy data between it and the SSH frontend.
///
/// A PTY is opened to control ipmitool, since it's designed to work with one, and having a
/// persistent PTY allows multiple connections to work without worrying about how to interpret
/// multiple client PTY requests.
///
/// `to_frontend_tx` is a [`russh::Channel`] to send data from ipmitool to the SSH frontend.
///
/// Returns a [`mpsc::Sender<ChannelMsg>`] that the frontend can use to send data to ipmitool.
pub(in crate::bmc) async fn spawn(
    connection_details: Arc<ConnectionDetails>,
    to_frontend_tx: broadcast::Sender<ToFrontendMessage>,
    config: Arc<Config>,
    metrics: Arc<BmcPoolMetrics>,
    cancel_token: CancellationToken,
) -> Result<Handle, SpawnError> {
    let (cancel_token, drop_guard) = fork_cancel_token(cancel_token);
    let (ready_tx, ready_rx) = oneshot::channel::<()>();
    let ready_tx = Some(ready_tx); // only send it once

    let machine_id = connection_details.machine_id;
    // Open a PTY to control ipmitool
    let OpenptyResult {
        master: pty_master,
        slave: pty_slave,
    } = io_util::alloc_pty(80, 24)?;
    let pty_master = AsyncFd::new(pty_master).expect("BUG: not in tokio runtime?");

    // Run `ipmitool sol activate` with the appropriate args
    let mut command = sol_activate_command(&connection_details, &config);
    command
        // connect stdin/stdout/stderr to the pty
        .stdin(
            pty_slave
                .try_clone()
                .map_err(|error| SpawnError::PtySetup {
                    reason: "error cloning pty fd for stdin",
                    error,
                })?,
        )
        .stdout(
            pty_slave
                .try_clone()
                .map_err(|error| SpawnError::PtySetup {
                    reason: "error cloning pty fd for stdout",
                    error,
                })?,
        )
        .stderr(
            pty_slave
                .try_clone()
                .map_err(|error| SpawnError::PtySetup {
                    reason: "error cloning pty fd for stderr",
                    error,
                })?,
        )
        // Set the xterm env var as a reasonable default.
        .env("TERM", "xterm");

    // Spawn ipmitool in the controlling pty
    set_controlling_terminal_on_exec(&mut command, pty_slave.as_raw_fd());
    let ipmitool_process = command
        .spawn()
        .map_err(|error| SpawnError::SpawningIpmitool { error })?;
    drop(command);
    drop(pty_slave);

    // Make a channel the frontend can use to send messages to us
    let (from_frontend_tx, from_frontend_rx) = mpsc::channel::<ToBmcMessage>(1);

    let mut ipmitool_proxy = IpmitoolMessageProxy {
        connection_details,
        config,
        ipmitool_process,
        output_buf: [0u8; 4096],
        captured_output: VecDeque::with_capacity(MAX_CAPTURED_IPMITOOL_OUTPUT_SIZE),
        cancel_token,
        pty_master,
        from_frontend_rx,
        to_frontend_tx,
        ready_tx,
        metrics,
        sol_session_operational: false,
        escape_was_pending: false,
        pending_line: PendingOutputLine::with_max_size(1024),
        connected_since: Utc::now(),
        bytes_received: 0,
        output_last_received: None,
    };

    // Send messages to/from ipmitool in the background
    let join_handle = tokio::spawn(async move {
        ipmitool_proxy
            .manage_ipmitool_process()
            .await
            .map_err(|error| SpawnError::ProcessLoop {
                error,
                output: ipmitool_proxy.captured_output_string(),
            })?;

        let exit_status = ipmitool_proxy
            .ipmitool_process
            .try_wait()
            .map_err(|error| SpawnError::CheckingIpmitoolExitStatus {
                error,
                output: ipmitool_proxy.captured_output_string(),
            })?;

        match exit_status {
            Some(exit_status) => {
                // Any exit from ipmitool is unexpected: It's supposed to run forever until we shut
                // it down. If explicitly configured, recover a conflicting SOL session by
                // deactivating it before the client retries the connection.
                let output = ipmitool_proxy.captured_output_string();
                Err(handle_unexpected_ipmitool_exit(
                    exit_status,
                    output,
                    ipmitool_proxy.sol_session_operational,
                    ipmitool_proxy
                        .config
                        .force_deactivate_conflicting_ipmi_sol_sessions,
                    || deactivate_sol(&ipmitool_proxy.connection_details, &ipmitool_proxy.config),
                )
                .await)
            }
            None => {
                // Process is still running (normal shutdown), we can kill it.
                tracing::debug!(%machine_id, "killing ipmitool process");
                // Kill and wait() on the process (to avoid zombies), but in the background (so we don't
                // block if it's unresponsive.)
                tokio::spawn(async move { ipmitool_proxy.ipmitool_process.kill().await });
                Ok(())
            }
        }
    });

    ready_rx.await.map_err(|_| SpawnError::WaitingForReady)?;

    Ok(Handle {
        to_bmc_msg_tx: from_frontend_tx,
        join_handle,
        drop_guard,
    })
}

/// A handle to a BMC connection, which will shut down when dropped.
pub(in crate::bmc) struct Handle {
    pub(in crate::bmc) to_bmc_msg_tx: mpsc::Sender<ToBmcMessage>,
    pub(in crate::bmc) join_handle: JoinHandle<Result<(), SpawnError>>,
    pub(in crate::bmc) drop_guard: DropGuard,
}

#[derive(thiserror::Error, Debug)]
pub(in crate::bmc) enum SpawnError {
    #[error("error spawning a PTY for ipmitool: {0}")]
    PtyAlloc(#[from] PtyAllocError),
    #[error("error setting up pty: {reason}: {error}")]
    PtySetup {
        reason: &'static str,
        error: std::io::Error,
    },
    #[error("error spawning ipmitool: {error}")]
    SpawningIpmitool { error: std::io::Error },
    #[error("error checking ipmitool exit status: {error}. output: {output}")]
    CheckingIpmitoolExitStatus {
        error: std::io::Error,
        output: String,
    },
    #[error("ipmitool exited unexpectedly: {exit_status}, output: {output}")]
    IpmitoolUnexpectedExit {
        exit_status: ExitStatus,
        output: String,
    },
    #[error(
        "conflicting IPMI SOL session was deactivated after ipmitool exited unexpectedly: {exit_status}, output: {output}"
    )]
    ConflictingSolSessionDeactivated {
        exit_status: ExitStatus,
        output: String,
    },
    #[error(
        "failed to deactivate conflicting IPMI SOL session after ipmitool exited unexpectedly: {exit_status}, activation output: {output}: {error}"
    )]
    ConflictingSolSessionDeactivationFailed {
        exit_status: ExitStatus,
        output: String,
        #[source]
        error: SolDeactivateError,
    },
    #[error("unknown error waiting for ipmitool to be ready")]
    WaitingForReady,
    #[error("error running ipmitool: {error}. output: {output}")]
    ProcessLoop {
        error: ProcessLoopError,
        output: String,
    },
}

impl SpawnError {
    pub(crate) fn retry_immediately(&self) -> bool {
        matches!(self, Self::ConflictingSolSessionDeactivated { .. })
    }
}

#[derive(thiserror::Error, Debug)]
pub(in crate::bmc) enum SolDeactivateError {
    #[error("error spawning ipmitool for SOL deactivation: {error}")]
    Spawning { error: std::io::Error },
    #[error("error waiting for ipmitool SOL deactivation: {error}")]
    Waiting { error: std::io::Error },
    #[error("ipmitool SOL deactivation timed out after {timeout:?}")]
    Timeout { timeout: Duration },
    #[error("ipmitool SOL deactivation failed with {exit_status}: {output}")]
    Failure {
        exit_status: ExitStatus,
        output: String,
    },
}

async fn handle_unexpected_ipmitool_exit<Deactivate, DeactivateFuture>(
    exit_status: ExitStatus,
    output: String,
    sol_session_operational: bool,
    force_deactivate_conflicting_ipmi_sol_sessions: bool,
    deactivate_sol: Deactivate,
) -> SpawnError
where
    Deactivate: FnOnce() -> DeactivateFuture,
    DeactivateFuture: Future<Output = Result<(), SolDeactivateError>>,
{
    if sol_session_operational
        || !force_deactivate_conflicting_ipmi_sol_sessions
        || !is_sol_payload_already_active(&output)
    {
        return SpawnError::IpmitoolUnexpectedExit {
            exit_status,
            output,
        };
    }

    match deactivate_sol().await {
        Ok(()) => SpawnError::ConflictingSolSessionDeactivated {
            exit_status,
            output,
        },
        Err(error) => SpawnError::ConflictingSolSessionDeactivationFailed {
            exit_status,
            output,
            error,
        },
    }
}

fn is_sol_payload_already_active(output: &str) -> bool {
    output.contains(SOL_PAYLOAD_ALREADY_ACTIVE)
}

async fn deactivate_sol(
    connection_details: &ConnectionDetails,
    config: &Config,
) -> Result<(), SolDeactivateError> {
    let machine_id = connection_details.machine_id;
    // The explicit opt-in asserts that ssh-console owns SOL exclusively, so recovery here
    // intentionally replaces any out-of-band session that prevents it from becoming the owner.
    tracing::warn!(
        %machine_id,
        "conflicting IPMI SOL session detected; deactivating it before reconnecting"
    );

    let result = run_sol_deactivate_command(
        sol_deactivate_command(connection_details, config),
        SOL_DEACTIVATE_TIMEOUT,
    )
    .await;

    if result.is_ok() {
        tracing::info!(
            %machine_id,
            "conflicting IPMI SOL session deactivated; retrying connection immediately"
        );
    }

    result
}

async fn run_sol_deactivate_command(
    mut command: tokio::process::Command,
    timeout: Duration,
) -> Result<(), SolDeactivateError> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| SolDeactivateError::Spawning { error })?;
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| SolDeactivateError::Timeout { timeout })?
        .map_err(|error| SolDeactivateError::Waiting { error })?;

    if output.status.success() {
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(SolDeactivateError::Failure {
            exit_status: output.status,
            output: format!("stdout: {stdout:?}, stderr: {stderr:?}"),
        })
    }
}

#[derive(thiserror::Error, Debug)]
pub(in crate::bmc) enum ProcessLoopError {
    #[error("error polling from pty master fd: {error}")]
    PollingFromPty { error: std::io::Error },
    #[error("error writing data from ipmitool to frontend channel: no active receivers")]
    WritingToFrontendChannel,
    #[error("error reading ipmitool output: {error}")]
    ReadingFromIpmitoolPty { error: std::io::Error },
    #[error("error waiting for ipmitool to exit: {error}")]
    WaitingForIpmitool { error: std::io::Error },
    #[error("error checking ipmitool after its pty closed: {error}")]
    CheckingIpmitoolAfterPtyClosed { error: std::io::Error },
    #[error("error killing ipmitool after its pty closed: {error}")]
    KillingIpmitoolAfterPtyClosed { error: std::io::Error },
    #[error("error sending frontend message to ipmi console: {0}")]
    SendingFrontendMessageToIpmiConsole(#[from] SendFrontendMessageToIpmiConsoleError),
    #[error("error resetting power: {0}")]
    PowerReset(#[from] PowerResetError),
}

#[derive(thiserror::Error, Debug)]
pub(in crate::bmc) enum SendFrontendMessageToIpmiConsoleError {
    #[error("error writing to ipmitool pty: {error}")]
    WritingToPty { error: std::io::Error },
}

#[derive(thiserror::Error, Debug)]
pub(in crate::bmc) enum PowerResetError {
    #[error("error spawning ipmitool for power reset: {error}")]
    Spawning { error: std::io::Error },
    #[error("ipmitool error running power reset: {error}")]
    Waiting { error: std::io::Error },
    #[error("ipmitool power reset failed: {output}")]
    Failure { output: String },
}

struct IpmitoolMessageProxy {
    connection_details: Arc<ConnectionDetails>,
    config: Arc<Config>,
    ipmitool_process: Child,
    output_buf: [u8; 4096],
    captured_output: VecDeque<u8>,
    cancel_token: CancellationToken,
    pty_master: AsyncFd<OwnedFd>,
    from_frontend_rx: mpsc::Receiver<ToBmcMessage>,
    to_frontend_tx: broadcast::Sender<ToFrontendMessage>,
    ready_tx: Option<oneshot::Sender<()>>,
    metrics: Arc<BmcPoolMetrics>,
    // Once ipmitool confirms activation, later console output must not trigger activation recovery.
    sol_session_operational: bool,
    // Keep track of whether the last byte sent from the client was the first byte of an escape sequence.
    escape_was_pending: bool,
    // Keep track of the last data we saw after a newline, so that we can replay it when clients join.
    pending_line: PendingOutputLine,
    // Keep track of bytes received, unfortunately we can't read from a Metrics object so we need to write to our own value.
    bytes_received: usize,
    // Keep track of when the connection started
    connected_since: DateTime<Utc>,
    output_last_received: Option<DateTime<Utc>>,
}

enum PtyReadResult {
    Data(usize),
    WouldBlock,
    Closed,
}

impl IpmitoolMessageProxy {
    /// Poll from the SSH frontend and the ipmitool PTY in the foreground, pumping messages between
    /// them, until either the frontend closes or ipmitool exits.
    ///
    /// This function is tricky because we're dealing with "normal" UNIX file descriptors (set with
    /// O_NONBLOCK), but we want to poll them in a tokio::select loop.  So we have to do the typical
    /// UNIX pattern of reading/writing data until we get EWOULDBLOCK, returning to the main loop, etc.
    async fn manage_ipmitool_process(&mut self) -> Result<(), ProcessLoopError> {
        let machine_id = self.connection_details.machine_id;
        let metrics_attrs = vec![KeyValue::new("machine_id", machine_id.to_string())];
        let mut ipmitool_exited = false;
        let mut pty_closed = false;
        let mut kill_requested = false;
        loop {
            tokio::select! {
                // Break if we're shut down
                _ = self.cancel_token.cancelled() => {
                    tracing::debug!("ipmitool_process_loop shutdown received");
                    break;
                }
                // Record the exit, then keep polling the PTY until all diagnostic output is drained.
                exit_status = self.ipmitool_process.wait(), if !ipmitool_exited => {
                    let exit_status = exit_status
                        .map_err(|error| ProcessLoopError::WaitingForIpmitool { error })?;
                    tracing::warn!(%machine_id, ?exit_status, "ipmitool exited");
                    ipmitool_exited = true;
                    loop {
                        let read_result = read_ipmitool_pty(
                            self.pty_master.get_ref(),
                            &mut self.output_buf,
                        )
                        .inspect_err(|_| {
                            self.metrics
                                .bmc_rx_errors_total
                                .add(1, metrics_attrs.as_slice());
                        })?;
                        match read_result {
                            PtyReadResult::Data(n) => {
                                self.handle_ipmitool_pty_data(n, &metrics_attrs)?;
                            }
                            PtyReadResult::WouldBlock | PtyReadResult::Closed => {
                                pty_closed = true;
                                break;
                            }
                        }
                    }
                }
                // Poll for any data to be available in pty_master
                guard = self.pty_master.readable(), if !pty_closed => {
                    let mut guard = guard.map_err(|error| ProcessLoopError::PollingFromPty { error })?;
                    let read_result = read_ipmitool_pty(guard.get_inner(), &mut self.output_buf)
                        .inspect_err(|_| {
                            self.metrics.bmc_rx_errors_total.add(1, metrics_attrs.as_slice());
                        })?;
                    match read_result {
                        PtyReadResult::Data(n) => {
                            drop(guard);
                            self.handle_ipmitool_pty_data(n, &metrics_attrs)?;
                            // Keep the readiness set so the next loop drains any remaining data.
                        }
                        PtyReadResult::WouldBlock => {
                            // clear the readiness so we go back to polling
                            guard.clear_ready();
                        }
                        PtyReadResult::Closed => {
                            tracing::debug!(%machine_id, "eof from closed ipmitool pty");
                            pty_closed = true;
                        }
                    }
                }
                // Poll for any messages from the SSH frontend
                res = self.from_frontend_rx.recv(), if !pty_closed => match res {
                    Some(msg) => {
                        self.send_frontend_message_to_ipmi_console(msg).await.inspect_err(|_| {
                            self.metrics.bmc_tx_errors_total.add(1, metrics_attrs.as_slice());
                        })?;
                    }
                    None => {
                        tracing::info!(%machine_id, "all frontend connections closed, stopping ipmitool");
                        break;
                    }
                },
            }

            if pty_closed && !ipmitool_exited {
                match self.ipmitool_process.try_wait() {
                    Ok(Some(exit_status)) => {
                        tracing::warn!(%machine_id, ?exit_status, "ipmitool exited after closing its pty");
                        ipmitool_exited = true;
                    }
                    Ok(None) if !kill_requested => {
                        self.ipmitool_process.start_kill().map_err(|error| {
                            ProcessLoopError::KillingIpmitoolAfterPtyClosed { error }
                        })?;
                        kill_requested = true;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return Err(ProcessLoopError::CheckingIpmitoolAfterPtyClosed { error });
                    }
                }
            }

            if pty_closed && ipmitool_exited {
                break;
            }
        }

        Ok(())
    }

    fn handle_ipmitool_pty_data(
        &mut self,
        n: usize,
        metrics_attrs: &[KeyValue],
    ) -> Result<(), ProcessLoopError> {
        let data = &self.output_buf[0..n];
        capture_ipmitool_startup_output(
            &mut self.captured_output,
            &mut self.sol_session_operational,
            data,
        );
        // ipmitool always emits a message after either connecting or rejecting activation.
        if let Some(ready_tx) = self.ready_tx.take() {
            self.connected_since = Utc::now();
            ready_tx.send(()).ok();
        }
        self.output_last_received = Some(Utc::now());
        self.metrics
            .bmc_bytes_received_total
            .add(n as _, metrics_attrs);
        self.bytes_received += n;
        self.pending_line.extend(data);
        self.to_frontend_tx
            .send(ToFrontendMessage::Channel(Arc::new(ChannelMsg::Data {
                data: data.to_vec().into(),
            })))
            .map_err(|_| ProcessLoopError::WritingToFrontendChannel)?;

        Ok(())
    }

    async fn send_frontend_message_to_ipmi_console(
        &mut self,
        msg: ToBmcMessage,
    ) -> Result<(), SendFrontendMessageToIpmiConsoleError> {
        let machine_id = self.connection_details.machine_id;
        let msg = match msg {
            // Filter out escape sequences
            ToBmcMessage::ChannelMsg(
                ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, ext: _ },
            ) => {
                let (data, escape_pending) = IPMITOOL_ESCAPE_SEQUENCE
                    .filter_escape_sequences(data.as_ref(), self.escape_was_pending);
                self.escape_was_pending = escape_pending;
                ToBmcMessage::ChannelMsg(ChannelMsg::Data {
                    data: data.into_owned().into(),
                })
            }
            msg => msg,
        };

        match msg {
            ToBmcMessage::ChannelMsg(ChannelMsg::Eof | ChannelMsg::Close) => {
                // multiple clients can come and go, we don't close just because one of them disconnected.
            }
            ToBmcMessage::ChannelMsg(ChannelMsg::Data { data }) => {
                write_data_to_async_fd(&data, &self.pty_master)
                    .await
                    .map_err(
                        |error| SendFrontendMessageToIpmiConsoleError::WritingToPty { error },
                    )?;
            }
            ToBmcMessage::ChannelMsg(ChannelMsg::WindowChange {
                col_width,
                row_height,
                pix_width,
                pix_height,
            }) => {
                // update the kernel pty size
                let winsz = libc::winsize {
                    ws_row: row_height.try_into().unwrap_or(80),
                    ws_col: col_width.try_into().unwrap_or(24),
                    ws_xpixel: pix_width.try_into().unwrap_or(0),
                    ws_ypixel: pix_height.try_into().unwrap_or(0),
                };
                // SAFETY: `pty_master` owns a valid PTY descriptor, and the fully initialized
                // `winsz` value has the layout and lifetime required by `TIOCSWINSZ`.
                unsafe {
                    libc::ioctl(self.pty_master.as_raw_fd(), libc::TIOCSWINSZ, &winsz);
                }
            }
            ToBmcMessage::Exec { command, reply_tx } => match String::from_utf8(command) {
                Ok(command) if command == POWER_RESET_COMMAND => match self.power_reset().await {
                    Ok(()) => {
                        reply_tx
                            .send(ExecReply {
                                output: b"Power reset completed successfully\r\n".to_vec(),
                                exit_status: 0,
                            })
                            .ok();
                    }
                    Err(e) => {
                        reply_tx
                            .send(ExecReply {
                                output: format!("{e}\r\n").into_bytes(),
                                exit_status: 1,
                            })
                            .ok();
                    }
                },
                _ => {
                    reply_tx
                        .send(ExecReply {
                            output: b"Unsupported command\r\n".as_slice().into(),
                            exit_status: 127,
                        })
                        .ok();
                }
            },
            ToBmcMessage::EchoConnectionMessage { reply_tx } => {
                echo_connected_message(
                    reply_tx,
                    &self.pending_line,
                    self.bytes_received,
                    self.output_last_received,
                    self.connected_since,
                );
                return Ok(());
            }
            other => {
                tracing::debug!(
                    %machine_id,
                    ?other,
                    "Not handling unknown SSH frontend message in ipmitool"
                );
            }
        };
        Ok(())
    }

    async fn power_reset(&mut self) -> Result<(), PowerResetError> {
        let mut command = ipmitool_command(&self.connection_details, &self.config);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        command.arg("power").arg("reset");

        let output = command
            .spawn()
            .map_err(|error| PowerResetError::Spawning { error })?
            .wait_with_output()
            .await
            .map_err(|error| PowerResetError::Waiting { error })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(PowerResetError::Failure {
                output: String::from_utf8_lossy(&output.stderr).to_string(),
            })
        }
    }

    fn captured_output_string(&self) -> String {
        let output: Vec<_> = self.captured_output.iter().copied().collect();
        String::from_utf8_lossy(&output).into_owned()
    }
}

fn read_ipmitool_pty(
    pty_master: &OwnedFd,
    output_buf: &mut [u8],
) -> Result<PtyReadResult, ProcessLoopError> {
    match unistd::read(pty_master, output_buf) {
        Ok(0) | Err(Errno::EIO) => Ok(PtyReadResult::Closed),
        Ok(n) => Ok(PtyReadResult::Data(n)),
        Err(Errno::EWOULDBLOCK) => Ok(PtyReadResult::WouldBlock),
        Err(error) => Err(std::io::Error::from_raw_os_error(error as _))
            .map_err(|error| ProcessLoopError::ReadingFromIpmitoolPty { error }),
    }
}

fn append_captured_output(output: &mut VecDeque<u8>, data: &[u8]) {
    output.extend(data.iter().copied());
    let excess = output
        .len()
        .saturating_sub(MAX_CAPTURED_IPMITOOL_OUTPUT_SIZE);
    output.drain(..excess);
}

fn capture_ipmitool_startup_output(
    output: &mut VecDeque<u8>,
    sol_session_operational: &mut bool,
    data: &[u8],
) {
    if *sol_session_operational {
        return;
    }

    append_captured_output(output, data);
    if captured_output_contains(output, SOL_SESSION_OPERATIONAL) {
        *sol_session_operational = true;
        output.clear();
    }
}

fn captured_output_contains(output: &VecDeque<u8>, needle: &[u8]) -> bool {
    !needle.is_empty()
        && needle.len() <= output.len()
        && (0..=output.len() - needle.len()).any(|start| {
            output
                .iter()
                .skip(start)
                .take(needle.len())
                .copied()
                .eq(needle.iter().copied())
        })
}

fn configure_ipmitool_connection(
    command: &mut tokio::process::Command,
    connection_details: &ConnectionDetails,
) {
    command
        .arg("-I")
        .arg("lanplus")
        .arg("-H")
        .arg(connection_details.addr.ip().to_string())
        .arg("-p")
        .arg(connection_details.addr.port().to_string())
        .arg("-U")
        .arg(&connection_details.user)
        .arg("-E")
        .env(IPMITOOL_PASSWORD_ENV_VAR, &connection_details.password);
}

fn ipmitool_command(
    connection_details: &ConnectionDetails,
    config: &Config,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("ipmitool");
    configure_ipmitool_connection(&mut command, connection_details);

    if config.insecure_ipmi_ciphers {
        command.arg("-C").arg("3"); // use SHA1 ciphers, useful for ipmi_sim
    }

    command
}

fn sol_deactivate_command(
    connection_details: &ConnectionDetails,
    config: &Config,
) -> tokio::process::Command {
    let mut command = ipmitool_command(connection_details, config);
    command.arg("sol").arg("deactivate");
    command
}

fn sol_activate_command(
    connection_details: &ConnectionDetails,
    config: &Config,
) -> tokio::process::Command {
    let mut command = ipmitool_command(connection_details, config);
    command.arg("sol").arg("activate");
    command
}

#[derive(Clone)]
pub(in crate::bmc) struct ConnectionDetails {
    pub(in crate::bmc) machine_id: MachineId,
    pub(in crate::bmc) addr: SocketAddr,
    pub(in crate::bmc) user: String,
    pub(in crate::bmc) password: String,
}

impl Debug for ConnectionDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Skip writing the password
        f.debug_struct("IpmiConnectionDetails")
            .field("addr", &self.addr)
            .field("user", &self.user)
            .field("machine_id", &self.machine_id.to_string())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::os::unix::process::ExitStatusExt;
    use std::sync::atomic::{AtomicBool, Ordering};

    use carbide_test_support::value_scenarios;
    use carbide_uuid::machine::{MachineIdSource, MachineType};

    use super::*;

    #[test]
    fn sol_payload_already_active_output_is_detected_narrowly() {
        value_scenarios!(
            run = |output: &str| is_sol_payload_already_active(output);

            "conflicting SOL session" {
                "Info: SOL payload already active on another session" => true,
                "prefix\r\nInfo: SOL payload already active on another session\r\nsuffix" => true,
            }

            "unrelated ipmitool output" {
                "" => false,
                "Error: Unable to establish IPMI v2 / RMCP+ session" => false,
                "Info: SOL payload disabled" => false,
                "Info: SOL payload activation limit reached" => false,
                "Info: SOL payload already de-activated" => false,
            }
        );
    }

    #[tokio::test]
    async fn unrelated_ipmitool_exit_does_not_deactivate_sol() {
        let deactivation_called = AtomicBool::new(false);

        let error = handle_unexpected_ipmitool_exit(
            failed_exit_status(),
            "authentication failed".to_string(),
            false,
            true,
            || async {
                deactivation_called.store(true, Ordering::Relaxed);
                Ok(())
            },
        )
        .await;

        assert!(matches!(&error, SpawnError::IpmitoolUnexpectedExit { .. }));
        assert!(!deactivation_called.load(Ordering::Relaxed));
        assert!(!error.retry_immediately());
    }

    #[tokio::test]
    async fn conflicting_sol_session_is_not_deactivated_by_default() {
        let deactivation_called = AtomicBool::new(false);

        let error = handle_unexpected_ipmitool_exit(
            failed_exit_status(),
            format!("Info: {SOL_PAYLOAD_ALREADY_ACTIVE}\r\n"),
            false,
            false,
            || async {
                deactivation_called.store(true, Ordering::Relaxed);
                Ok(())
            },
        )
        .await;

        assert!(matches!(&error, SpawnError::IpmitoolUnexpectedExit { .. }));
        assert!(!deactivation_called.load(Ordering::Relaxed));
        assert!(!error.retry_immediately());
    }

    #[tokio::test]
    async fn conflicting_sol_session_is_deactivated_and_retried_immediately_when_enabled() {
        let error = handle_unexpected_ipmitool_exit(
            failed_exit_status(),
            format!("Info: {SOL_PAYLOAD_ALREADY_ACTIVE}\r\n"),
            false,
            true,
            || async { Ok(()) },
        )
        .await;

        assert!(matches!(
            &error,
            SpawnError::ConflictingSolSessionDeactivated { .. }
        ));
        assert!(error.retry_immediately());
    }

    #[tokio::test]
    async fn failed_sol_deactivation_preserves_normal_retry_backoff() {
        let error = handle_unexpected_ipmitool_exit(
            failed_exit_status(),
            format!("Info: {SOL_PAYLOAD_ALREADY_ACTIVE}\r\n"),
            false,
            true,
            || async {
                Err(SolDeactivateError::Failure {
                    exit_status: failed_exit_status(),
                    output: "deactivation failed".to_string(),
                })
            },
        )
        .await;

        assert!(matches!(
            &error,
            SpawnError::ConflictingSolSessionDeactivationFailed { .. }
        ));
        assert!(!error.retry_immediately());
        assert!(error.to_string().contains("deactivation failed"));
    }

    #[tokio::test]
    async fn established_sol_session_never_treats_console_output_as_activation_failure() {
        let deactivation_called = AtomicBool::new(false);
        let error = handle_unexpected_ipmitool_exit(
            failed_exit_status(),
            format!("host output: {SOL_PAYLOAD_ALREADY_ACTIVE}\r\n"),
            true,
            true,
            || async {
                deactivation_called.store(true, Ordering::Relaxed);
                Ok(())
            },
        )
        .await;

        assert!(matches!(&error, SpawnError::IpmitoolUnexpectedExit { .. }));
        assert!(!deactivation_called.load(Ordering::Relaxed));
        assert!(!error.retry_immediately());
    }

    #[test]
    fn startup_output_capture_is_bounded_and_stops_after_sol_becomes_operational() {
        let mut output = VecDeque::new();
        let mut sol_session_operational = false;
        capture_ipmitool_startup_output(
            &mut output,
            &mut sol_session_operational,
            &vec![b'x'; MAX_CAPTURED_IPMITOOL_OUTPUT_SIZE + 1],
        );
        assert_eq!(output.len(), MAX_CAPTURED_IPMITOOL_OUTPUT_SIZE);
        assert!(output.iter().all(|byte| *byte == b'x'));

        output.clear();
        capture_ipmitool_startup_output(
            &mut output,
            &mut sol_session_operational,
            b"prefix SOL Session oper",
        );
        assert!(!sol_session_operational);

        capture_ipmitool_startup_output(
            &mut output,
            &mut sol_session_operational,
            b"ational. Use ~? for help\r\n",
        );
        assert!(sol_session_operational);
        assert!(output.is_empty());

        capture_ipmitool_startup_output(
            &mut output,
            &mut sol_session_operational,
            b"post-connect host console data",
        );
        assert!(output.is_empty());
    }

    #[tokio::test]
    async fn exited_process_output_is_drained_from_the_pty() {
        let OpenptyResult {
            master: pty_master,
            slave: pty_slave,
        } = io_util::alloc_pty(80, 24).expect("allocate pty");
        let pty_master = AsyncFd::new(pty_master).expect("register pty with tokio");
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(format!(
                "printf 'Info: {SOL_PAYLOAD_ALREADY_ACTIVE}\\r\\n'; exit 1"
            ))
            .stdin(Stdio::null())
            .stdout(pty_slave.try_clone().expect("clone pty for stdout"))
            .stderr(pty_slave.try_clone().expect("clone pty for stderr"));
        let ipmitool_process = command.spawn().expect("spawn fake ipmitool");
        drop(command);
        drop(pty_slave);

        let cancel_token = CancellationToken::new();
        let (_from_frontend_tx, from_frontend_rx) = mpsc::channel(1);
        let (to_frontend_tx, _to_frontend_rx) = broadcast::channel(8);
        let (ready_tx, mut ready_rx) = oneshot::channel();
        let mut proxy = IpmitoolMessageProxy {
            connection_details: Arc::new(connection_details()),
            config: Arc::new(Config::default()),
            ipmitool_process,
            output_buf: [0; 4096],
            captured_output: VecDeque::with_capacity(MAX_CAPTURED_IPMITOOL_OUTPUT_SIZE),
            cancel_token: cancel_token.clone(),
            pty_master,
            from_frontend_rx,
            to_frontend_tx,
            ready_tx: Some(ready_tx),
            metrics: Arc::new(BmcPoolMetrics::for_test()),
            sol_session_operational: false,
            escape_was_pending: false,
            pending_line: PendingOutputLine::with_max_size(1024),
            bytes_received: 0,
            connected_since: Utc::now(),
            output_last_received: None,
        };

        tokio::time::timeout(Duration::from_secs(2), proxy.manage_ipmitool_process())
            .await
            .expect("PTY drain should not hang")
            .expect("PTY drain should succeed");

        assert!(
            proxy
                .captured_output_string()
                .contains(SOL_PAYLOAD_ALREADY_ACTIVE)
        );
        assert!(!proxy.sol_session_operational);
        assert!(
            ready_rx.try_recv().is_ok(),
            "diagnostic output should mark the process ready"
        );
        assert_eq!(
            proxy
                .ipmitool_process
                .try_wait()
                .expect("read child status")
                .and_then(|status| status.code()),
            Some(1)
        );
    }

    #[test]
    fn sol_commands_use_the_same_connection_details_and_cipher_configuration() {
        let connection_details = connection_details();
        let config = Config {
            insecure_ipmi_ciphers: true,
            ..Default::default()
        };
        let expected_common_args = [
            "-I",
            "lanplus",
            "-H",
            "192.0.2.10",
            "-p",
            "1623",
            "-U",
            "admin",
            "-E",
            "-C",
            "3",
            "sol",
        ];

        for (scenario, command, action) in [
            (
                "activation",
                sol_activate_command(&connection_details, &config),
                "activate",
            ),
            (
                "deactivation",
                sol_deactivate_command(&connection_details, &config),
                "deactivate",
            ),
        ] {
            assert_eq!(command.as_std().get_program(), OsStr::new("ipmitool"));
            let args: Vec<_> = command
                .as_std()
                .get_args()
                .map(|arg| arg.to_str().expect("ipmitool arguments should be UTF-8"))
                .collect();
            let expected_args: Vec<_> = expected_common_args
                .iter()
                .copied()
                .chain(std::iter::once(action))
                .collect();
            assert_eq!(args, expected_args, "{scenario}");
            assert!(!args.contains(&"-P"), "{scenario}");
            assert!(!args.contains(&"password"), "{scenario}");

            let password_env = command.as_std().get_envs().find_map(|(key, value)| {
                if key == OsStr::new(IPMITOOL_PASSWORD_ENV_VAR) {
                    value.and_then(OsStr::to_str)
                } else {
                    None
                }
            });
            assert_eq!(password_env, Some("password"), "{scenario}");
        }

        let command = sol_deactivate_command(&connection_details, &Config::default());
        assert!(
            !command
                .as_std()
                .get_args()
                .any(|arg| arg == OsStr::new("-C")),
            "secure defaults should not force the test-only cipher suite"
        );
    }

    #[tokio::test]
    async fn sol_deactivate_command_accepts_successful_exit() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("exit 0");

        run_sol_deactivate_command(command, SOL_DEACTIVATE_TIMEOUT)
            .await
            .expect("successful command should be accepted");
    }

    #[tokio::test]
    async fn sol_deactivate_command_reports_exit_output() {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("printf 'standard output'; printf 'standard error' >&2; exit 7");

        let error = run_sol_deactivate_command(command, SOL_DEACTIVATE_TIMEOUT)
            .await
            .expect_err("failed command should be reported");

        match error {
            SolDeactivateError::Failure {
                exit_status,
                output,
            } => {
                assert_eq!(exit_status.code(), Some(7));
                assert!(output.contains("standard output"));
                assert!(output.contains("standard error"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn sol_deactivate_command_reports_spawn_failure() {
        let command = tokio::process::Command::new("/path/that/does/not/exist/ipmitool");

        let error = run_sol_deactivate_command(command, SOL_DEACTIVATE_TIMEOUT)
            .await
            .expect_err("missing executable should be reported");

        assert!(matches!(error, SolDeactivateError::Spawning { .. }));
    }

    #[tokio::test]
    async fn sol_deactivate_command_reports_timeout() {
        let mut command = tokio::process::Command::new("sleep");
        command.arg("60");
        let timeout = Duration::from_millis(10);

        let error = run_sol_deactivate_command(command, timeout)
            .await
            .expect_err("hung command should time out");

        assert!(matches!(
            error,
            SolDeactivateError::Timeout { timeout: actual } if actual == timeout
        ));
    }

    fn connection_details() -> ConnectionDetails {
        ConnectionDetails {
            machine_id: MachineId::new(MachineIdSource::Tpm, [0; 32], MachineType::Host),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 1623),
            user: "admin".to_string(),
            password: "password".to_string(),
        }
    }

    fn failed_exit_status() -> ExitStatus {
        ExitStatus::from_raw(256)
    }

    #[test]
    fn configure_ipmitool_connection_passes_password_through_environment() {
        let connection_details = ConnectionDetails {
            machine_id: MachineId::new(MachineIdSource::Tpm, [0; 32], MachineType::Host),
            addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 623),
            user: "admin".to_string(),
            password: "hunter2".to_string(),
        };
        let mut command = tokio::process::Command::new("ipmitool");

        configure_ipmitool_connection(&mut command, &connection_details);

        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().expect("ipmitool args should be valid UTF-8"))
            .collect();
        let expected_args = [
            "-I",
            "lanplus",
            "-H",
            "127.0.0.1",
            "-p",
            "623",
            "-U",
            "admin",
            "-E",
        ];
        assert_eq!(args.as_slice(), expected_args.as_slice());
        assert!(!args.contains(&"-P"));
        assert!(!args.contains(&"hunter2"));

        let password_env = command.as_std().get_envs().find_map(|(key, value)| {
            if key == OsStr::new(IPMITOOL_PASSWORD_ENV_VAR) {
                value.and_then(OsStr::to_str)
            } else {
                None
            }
        });
        assert_eq!(password_env, Some("hunter2"));
    }
}
