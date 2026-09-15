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

use std::fmt::Debug;
use std::io::Read;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use carbide_uuid::machine::MachineId;
use chrono::Utc;
use opentelemetry::KeyValue;
use ringbuf::LocalRb;
use ringbuf::storage::Array;
use ringbuf::traits::RingBuffer;
use russh::client::{AuthResult, GexParams, KeyboardInteractiveAuthResponse};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey};
use russh::{Channel, ChannelMsg, MethodKind};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::POWER_RESET_COMMAND;
use crate::bmc::client_pool::BmcPoolMetrics;
use crate::bmc::connection_impl::echo_connected_message;
use crate::bmc::message_proxy::{
    ExecReply, MessageProxyError, ToBmcMessage, ToFrontendMessage, proxy_channel_message,
};
use crate::bmc::pending_output_line::PendingOutputLine;
use crate::bmc::vendor::SshBmcVendor;

static RUSSH_CLIENT_CONFIG: LazyLock<Arc<russh::client::Config>> =
    LazyLock::new(russh_client_config);
const LENOVO_SOL_ACTIVATION_RESPONSE_GRACE: Duration = Duration::from_secs(2);

/// Connect to a BMC one time, returning a [`Handle`]. Will not retry on connection errors.
pub(in crate::bmc) async fn spawn(
    connection_details: Arc<ConnectionDetails>,
    to_frontend_tx: broadcast::Sender<ToFrontendMessage>,
    metrics: Arc<BmcPoolMetrics>,
) -> Result<Handle, SpawnError> {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
    let (to_bmc_msg_tx, mut to_bmc_msg_rx) = mpsc::channel::<ToBmcMessage>(1);
    let metrics_attrs = vec![KeyValue::new(
        "machine_id",
        connection_details.machine_id.to_string(),
    )];

    let machine_id = connection_details.machine_id;
    let bmc_vendor = connection_details.bmc_vendor;

    let bmc_ssh_client = make_authenticated_client(&connection_details).await?;
    let connected_since = Utc::now();

    // Channel to send data to/from the BMC
    let mut ssh_client_channel = bmc_ssh_client
        .channel_open_session()
        .await
        .map_err(|error| SpawnError::OpeningSession { error })?;

    tracing::info!(%machine_id, "BMC SSH connection has established");
    trigger_and_await_sol_console(machine_id, &mut ssh_client_channel, bmc_vendor).await?;
    tracing::info!(%machine_id, "SOL console setup completed");

    let mut output_ringbuf: LocalRb<Array<u8, 1024>> = ringbuf::LocalRb::default();
    let bmc_prompt = bmc_vendor.bmc_prompt();
    let mut prior_escape_pending = false;
    let mut bytes_received = 0usize;
    let mut output_last_received = None;

    let join_handle = tokio::spawn(async move {
        let (mut ssh_client_rx, ssh_client_tx) = ssh_client_channel.split();
        let mut pending_line = PendingOutputLine::with_max_size(1024);

        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    tracing::info!(%machine_id, "BMC connection shutting down");
                    break;
                }
                res = ssh_client_rx.wait() => match res {
                    // Data coming from the BMC to the frontend
                    Some(msg) => {
                        if let ChannelMsg::Data { data, .. } = &msg {
                            pending_line.extend(data);
                            metrics.bmc_bytes_received_total.add(data.len() as _, metrics_attrs.as_slice());
                            bytes_received += data.len();
                            output_last_received = Some(Utc::now());
                            output_ringbuf.push_iter_overwrite(data.iter().copied());
                            if let Some(bmc_prompt) = bmc_prompt
                                && ringbuf_contains(&output_ringbuf, bmc_prompt) {
                                    let mut ringbuf_str = String::new();
                                    output_ringbuf.read_to_string(&mut ringbuf_str).ok();
                                    tracing::warn!(
                                        %machine_id,
                                        output = ?ringbuf_str,
                                        "BMC dropped to system prompt, exiting"
                                    );
                                    break;
                                }
                        }
                        to_frontend_tx.send(ToFrontendMessage::Channel(Arc::new(msg))).map_err(|_| SpawnError::SendingMsgToFrontend)?;
                    }
                    None => {
                        metrics.bmc_rx_errors_total.add(1, metrics_attrs.as_slice());
                        tracing::debug!(%machine_id, "BMC channel closed, closing connection");
                        break;
                    }
                },

                res = to_bmc_msg_rx.recv() => match res {
                    Some(msg) => {
                        let msg = match msg {
                            ToBmcMessage::ChannelMsg(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, ..}) => {
                                let (data, escape_pending) = bmc_vendor.filter_escape_sequences(data.as_ref(), prior_escape_pending);
                                prior_escape_pending = escape_pending;
                                ToBmcMessage::ChannelMsg(ChannelMsg::Data { data: data.into_owned().into() })
                            }
                            msg => msg,
                        };
                        let msg = match msg {
                            ToBmcMessage::ChannelMsg(msg) => msg,
                            ToBmcMessage::Exec { command, reply_tx} => {
                                let command = String::from_utf8(command);
                                match command {
                                    Ok(command) if command == POWER_RESET_COMMAND => {
                                        reply_tx.send(ExecReply {
                                            output: b"This BMC does not support power reset\r\n".to_vec(),
                                            exit_status: 1,
                                        }).ok();
                                    }
                                    _ => {
                                        reply_tx.send(ExecReply {
                                            output: b"Unsupported command\r\n".to_vec(),
                                            exit_status: 1,
                                        }).ok();
                                    }
                                }
                                continue;
                            }
                            ToBmcMessage::EchoConnectionMessage { reply_tx } => {
                                echo_connected_message(reply_tx, &pending_line, bytes_received, output_last_received, connected_since);
                                continue;
                            }
                        };
                        proxy_channel_message(&msg, &ssh_client_tx)
                            .await
                            .inspect_err(|_| {
                            metrics.bmc_tx_errors_total.add(1, metrics_attrs.as_slice());
                        }).map_err(|error| SpawnError::MessageProxying { error })?;
                    }
                    None => {
                        tracing::debug!(%machine_id, "frontend channel closed, closing connection");
                        break;
                    }
                },
            }
        }
        Ok(())
    });

    Ok(Handle {
        to_bmc_msg_tx,
        shutdown_tx,
        join_handle,
    })
}

/// A handle to a BMC connection, which will shut down when dropped.
pub(in crate::bmc) struct Handle {
    pub(in crate::bmc) to_bmc_msg_tx: mpsc::Sender<ToBmcMessage>,
    pub(in crate::bmc) shutdown_tx: oneshot::Sender<()>,
    pub(in crate::bmc) join_handle: JoinHandle<Result<(), SpawnError>>,
}

#[derive(thiserror::Error, Debug)]
pub(in crate::bmc) enum SpawnError {
    #[error("error sending message from BMC to frontend: no active receivers")]
    SendingMsgToFrontend,
    #[error("error connecting to SSH BMC: {0}")]
    ClientCreation(#[from] ClientCreationError),
    #[error("error opening session to SSH BMC: {error}")]
    OpeningSession { error: russh::Error },
    #[error("error activating serial console: {0}")]
    ConsoleActivation(#[from] ConsoleActivateError),
    #[error("error proxying message to BMC: {error}")]
    MessageProxying { error: MessageProxyError },
}

#[derive(thiserror::Error, Debug)]
pub(in crate::bmc) enum ClientCreationError {
    #[error("error connecting to {addr}: {error}")]
    Connection {
        addr: SocketAddr,
        error: russh::Error,
    },
    #[error("error beginning authentication to {addr}: {error}")]
    Authentication {
        addr: SocketAddr,
        error: russh::Error,
    },
    #[error("error loading SSH key from BMC override at {path}: {error}")]
    LoadingSshKey {
        path: String,
        error: russh::keys::Error,
    },
    #[error("error attempting {kind} authentication as {user} to {addr}: {error}")]
    AuthenticationAttempt {
        kind: &'static str,
        user: String,
        addr: SocketAddr,
        error: russh::Error,
    },

    #[error("could not authenticate to {addr} as {user}, all authentication attempts failed")]
    AuthenticationFailed { user: String, addr: SocketAddr },

    #[error("error sending message to BMC: {0}")]
    SendingMessageToBmc(#[from] MessageProxyError),
}

#[derive(thiserror::Error, Debug)]
pub(in crate::bmc) enum ConsoleActivateError {
    #[error("error while {phase}: {error}")]
    Request {
        phase: &'static str,
        error: russh::Error,
    },
    #[error("unable to activate serial console after timeout")]
    Timeout,
    #[error("BMC returned to its command prompt while activating serial console")]
    ReturnedToBmcPrompt,
}

/// Builds and authenticates an SSH client to a machine, using credentials from carbide-api or
/// overridden by config.
async fn make_authenticated_client(
    ConnectionDetails {
        addr,
        user,
        password,
        ssh_key_path,
        machine_id,
        ..
    }: &ConnectionDetails,
) -> Result<russh::client::Handle<Handler>, ClientCreationError> {
    let mut client = russh::client::connect(RUSSH_CLIENT_CONFIG.clone(), addr, Handler)
        .await
        .map_err(|error| ClientCreationError::Connection { addr: *addr, error })?;

    // Use authenticate_none to get a list of methods to try
    let methods = match client
        .authenticate_none(user)
        .await
        .map_err(|error| ClientCreationError::Authentication { addr: *addr, error })?
    {
        AuthResult::Success => {
            tracing::warn!(%machine_id, bmc_address = %addr, %user, "auth_none succeeded, it shouldn't have!");
            return Ok(client);
        }
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
    };

    // Loop through each method in order of what the server wants us to try
    for method in methods.iter().copied() {
        match method {
            MethodKind::PublicKey => {
                let Some(ssh_key_path) = &ssh_key_path else {
                    tracing::debug!(
                        %machine_id,
                        "skipping PublicKey authentication as we do not have a configured public key to use"
                    );
                    continue;
                };

                let ssh_key = PrivateKeyWithHashAlg::new(
                    Arc::new(russh::keys::load_secret_key(ssh_key_path, None).map_err(
                        |error| ClientCreationError::LoadingSshKey {
                            path: ssh_key_path.display().to_string(),
                            error,
                        },
                    )?),
                    Some(HashAlg::Sha512),
                );
                match client
                    .authenticate_publickey(user, ssh_key)
                    .await
                    .map_err(|error| ClientCreationError::AuthenticationAttempt {
                        kind: "PublicKey",
                        user: user.to_owned(),
                        addr: *addr,
                        error,
                    })? {
                    AuthResult::Success => {
                        tracing::debug!(
                            %machine_id, %user, bmc_address = %addr,
                            "PublicKey authentication succeeded"
                        );
                        return Ok(client);
                    }
                    AuthResult::Failure { .. } => {
                        tracing::warn!(%machine_id, %user, bmc_address = %addr, "PublicKey authentication failed")
                    }
                }
            }
            MethodKind::KeyboardInteractive => {
                let mut response = client
                    .authenticate_keyboard_interactive_start(user, None)
                    .await
                    .map_err(|error| ClientCreationError::AuthenticationAttempt {
                        kind: "KeyboardInteractive",
                        user: user.to_owned(),
                        addr: *addr,
                        error,
                    })?;

                loop {
                    match &response {
                        KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                            response = client
                                .authenticate_keyboard_interactive_respond(
                                    prompts.iter().map(|_| password.to_string()).collect(),
                                )
                                .await
                                .map_err(|error| ClientCreationError::AuthenticationAttempt {
                                    kind: "KeyboardInteractive authentication response",
                                    user: user.to_owned(),
                                    addr: *addr,
                                    error,
                                })?;
                            // We may get multiple info requests, so we to do this in a loop
                            // until we get a success or failure.
                        }
                        KeyboardInteractiveAuthResponse::Success => {
                            tracing::debug!(
                                %machine_id, %user, bmc_address = %addr,
                                "KeyboardInteractive authentication succeeded"
                            );
                            return Ok(client);
                        }
                        KeyboardInteractiveAuthResponse::Failure { .. } => {
                            tracing::warn!(
                                %machine_id, %user, bmc_address = %addr,
                                "KeyboardInteractive authentication failed"
                            );
                            break;
                        }
                    }
                }
            }
            MethodKind::Password => {
                match client
                    .authenticate_password(user, password)
                    .await
                    .map_err(|error| ClientCreationError::AuthenticationAttempt {
                        kind: "Password",
                        user: user.to_owned(),
                        addr: *addr,
                        error,
                    })? {
                    AuthResult::Success => {
                        tracing::debug!(
                            %machine_id, %user, bmc_address = %addr,
                            "Password authentication succeeded"
                        );
                        return Ok(client);
                    }
                    AuthResult::Failure { .. } => {
                        tracing::warn!(%machine_id, %user, bmc_address = %addr, "Password authentication failed");
                    }
                }
            }
            other => {
                tracing::debug!(%machine_id, ?other, "Ignoring unsupported auth method")
            }
        }
    }

    Err(ClientCreationError::AuthenticationFailed {
        user: user.to_owned(),
        addr: *addr,
    })
}

// Interact with the serial-on-lan console within the BMC ssh session, calling the vendor's serial
// activation command (`connect com1`, etc), falling back when needed, and ensuring we're in the
// serial console before continuing.
async fn trigger_and_await_sol_console(
    machine_id: MachineId,
    ssh_client_channel: &mut Channel<russh::client::Msg>,
    bmc_vendor: SshBmcVendor,
) -> Result<(), ConsoleActivateError> {
    // BMC activation sequence:
    // - Send PTY and shell requests to establish terminal
    // - Send vendor-specific activation command
    // - Wait for command echo to confirm activation
    // - Only then allow client to use the console

    ssh_client_channel
        .request_pty(false, "xterm", 80, 24, 0, 0, &[])
        .await
        .map_err(|error| ConsoleActivateError::Request {
            phase: "sending pty request to BMC",
            error,
        })?;
    ssh_client_channel
        .request_shell(false)
        .await
        .map_err(|error| ConsoleActivateError::Request {
            phase: "sending shell request to BMC",
            error,
        })?;

    let Some(bmc_prompt) = bmc_vendor.bmc_prompt() else {
        // This vendor lets us get a console directly by SSH'ing in (e.g. a DPU.)
        return Ok(());
    };
    let Some(activate_command) = bmc_vendor.serial_activate_command() else {
        // All vendors in bmc_vendor.rs must either return Some for both bmc_prompt() and
        // serial_activate_command(), or None for both of them.
        panic!("BUG: vendor has a BMC prompt but not a serial_activate_command")
    };

    ssh_client_channel
        .data(b"\n".as_slice())
        .await
        .map_err(|error| ConsoleActivateError::Request {
            phase: "sending newline to BMC",
            error,
        })?;

    let mut prompt_buf: Vec<u8> = Vec::with_capacity(1024);
    let mut timeout = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    // After sending the activate command, wait for this much data to be read back (the command
    // itself echoing back, plus the prompt length) before continuing. (If we let the client use the
    // console before this, we get false positives about seeing a bmc prompt while we're supposed to
    // be in the console.)
    let mut skip_data_read_len = bmc_prompt.len() + activate_command.len();
    let mut fallback_activate_sent = false;
    let mut fallback_activate_commands: Option<&'static [&'static [u8]]> = None;
    let mut next_fallback_command_index = 0;
    let mut lenovo_confirmation_pending = false;
    let mut lenovo_confirmation_deadline = timeout;
    let mut lenovo_confirmation_command: Option<&'static [u8]> = None;

    let mut activation_step = SerialConsoleActivationStep::WaitingForBmcPrompt;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(timeout) => {
                return Err(ConsoleActivateError::Timeout);
            }
            _ = tokio::time::sleep_until(lenovo_confirmation_deadline), if lenovo_confirmation_pending => {
                let confirmation_command = lenovo_confirmation_command
                    .expect("BUG: pending Lenovo confirmation must have a command");
                let activation_output = output_after_last_command(&prompt_buf, confirmation_command)
                    .expect("BUG: pending Lenovo confirmation command must be in output");
                if activation_output
                    .windows(bmc_prompt.len())
                    .any(|window| window == bmc_prompt)
                {
                    return Err(ConsoleActivateError::ReturnedToBmcPrompt);
                }
                tracing::debug!(%machine_id, "Lenovo activation response grace period elapsed without a BMC prompt, letting client use console");
                break;
            }
            res = ssh_client_channel.wait() => {
                let Some(msg) = res else {
                    tracing::error!(%machine_id, "BMC ssh connection closed before entering serial-on-lan console");
                    break
                };
                match msg {
                    ChannelMsg::Data { data } => {
                        prompt_buf.append(&mut data.to_vec());

                        if matches!(activation_step, SerialConsoleActivationStep::WaitingForBmcPrompt) {
                            // Do we see the bmc prompt?
                            if prompt_buf.windows(bmc_prompt.len()).any(|window| window == bmc_prompt) {
                                // We saw the prompt, send the serial activate command (`connect com1`,
                                // etc) one byte at a time: This seems to work better with some
                                // consoles.
                                send_command_bytewise(
                                    ssh_client_channel,
                                    activate_command,
                                    "sending serial activate command to BMC",
                                )
                                .await?;
                                activation_step = SerialConsoleActivationStep::ActivateSent;
                                // Clear the prompt
                                prompt_buf.clear();
                            }
                        }

                        // If we've sent the activate command, wait for it to be echoed back to us
                        // before continuing. (If we let the client use the console before this, we
                        // get false positives about seeing a bmc prompt while we're supposed to be
                        // in the console.)
                        if matches!(activation_step, SerialConsoleActivationStep::ActivateSent)
                            && let Some(fallback_commands) = bmc_vendor
                                .fallback_serial_activate_commands_if_needed(
                                    &prompt_buf,
                                    fallback_activate_sent,
                                )
                        {
                            tracing::info!(
                                %machine_id,
                                "Primary SOL activation failed, trying fallback"
                            );
                            fallback_activate_sent = true;
                            fallback_activate_commands = Some(fallback_commands);
                            next_fallback_command_index = 0;
                            lenovo_confirmation_pending = false;
                            lenovo_confirmation_command = None;
                            let fallback_command = fallback_commands[next_fallback_command_index];
                            next_fallback_command_index += 1;
                            skip_data_read_len = bmc_prompt.len() + fallback_command.len();
                            timeout = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
                            send_command_bytewise(
                                ssh_client_channel,
                                fallback_command,
                                "sending fallback serial activate command to BMC",
                            )
                            .await?;
                            prompt_buf.clear();
                        }

                        if matches!(activation_step, SerialConsoleActivationStep::ActivateSent)
                            && let Some(fallback_commands) = fallback_activate_commands
                            && next_fallback_command_index < fallback_commands.len()
                            && prompt_buf.len() > skip_data_read_len
                            && prompt_buf.windows(bmc_prompt.len()).any(|window| window == bmc_prompt)
                        {
                            let fallback_command = fallback_commands[next_fallback_command_index];
                            next_fallback_command_index += 1;
                            lenovo_confirmation_pending = false;
                            lenovo_confirmation_command = None;
                            skip_data_read_len = bmc_prompt.len() + fallback_command.len();
                            timeout = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
                            send_command_bytewise(
                                ssh_client_channel,
                                fallback_command,
                                "sending fallback serial activate command to BMC",
                            )
                            .await?;
                            prompt_buf.clear();
                        }

                        let waiting_for_fallback_prompt = fallback_activate_commands
                            .is_some_and(|commands| next_fallback_command_index < commands.len());
                        let fallback_sequence_complete = fallback_activate_commands
                            .is_some_and(|commands| next_fallback_command_index == commands.len());
                        let final_activation_command = if fallback_sequence_complete
                            && let Some(fallback_commands) = fallback_activate_commands
                        {
                            fallback_commands[fallback_commands.len() - 1]
                        } else {
                            activate_command
                        };
                        let activation_output = if matches!(bmc_vendor, SshBmcVendor::Lenovo) {
                            output_after_last_command(&prompt_buf, final_activation_command)
                        } else {
                            Some(prompt_buf.as_slice())
                        };
                        if matches!(activation_step, SerialConsoleActivationStep::ActivateSent)
                            && !waiting_for_fallback_prompt
                            && let Some(activation_output) = activation_output
                        {
                            if matches!(bmc_vendor, SshBmcVendor::Lenovo) {
                                if activation_output
                                    .windows(bmc_prompt.len())
                                    .any(|window| window == bmc_prompt)
                                {
                                    return Err(ConsoleActivateError::ReturnedToBmcPrompt);
                                }
                                if !lenovo_confirmation_pending {
                                    lenovo_confirmation_pending = true;
                                    lenovo_confirmation_deadline = tokio::time::Instant::now()
                                        + LENOVO_SOL_ACTIVATION_RESPONSE_GRACE;
                                    lenovo_confirmation_command = Some(final_activation_command);
                                }
                            } else if bmc_vendor.should_accept_sol_activation_output(
                                activation_output,
                                skip_data_read_len,
                            ) {
                                tracing::debug!(%machine_id, "confirmed serial activate command sent, letting client use console");
                                break;
                            }
                        }
                    }
                    msg => {
                        tracing::debug!(
                            %machine_id,
                            bmc_message = ?msg,
                            "message from BMC while activating serial prompt"
                        )
                    }
                }
            }
        }
    }

    Ok(())
}

/// Return the output beginning at the last echo of the final command in a command sequence.
///
/// Lenovo's legacy activation sequence contains two newline-separated commands. Ignoring output
/// before the final command lets the expected prompt after `console kill 1` coexist with a
/// successful, silent `console 1` transition.
fn output_after_last_command<'a>(output: &'a [u8], command_sequence: &[u8]) -> Option<&'a [u8]> {
    let final_command = command_sequence
        .rsplit(|byte| *byte == b'\n')
        .find(|command| !command.is_empty())?;
    output
        .windows(final_command.len())
        .rposition(|window| window == final_command)
        .map(|command_offset| &output[command_offset..])
}

struct Handler;

impl russh::client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        // TODO: known_hosts support?
        Ok(true)
    }
}

/// Configuration for russh's SSH client connections
fn russh_client_config() -> Arc<russh::client::Config> {
    let russh_config = russh::client::Config {
        // Some BMC's use a Diffie-Hellman group size of 2048, which is not allowed by default.
        gex: GexParams::new(2048, 8192, 8192)
            .expect("BUG: static DH group parameters must be valid"),
        keepalive_interval: Some(Duration::from_secs(60)),
        keepalive_max: 2,
        ..Default::default()
    };
    Arc::new(russh_config)
}

enum SerialConsoleActivationStep {
    WaitingForBmcPrompt,
    ActivateSent,
}

async fn send_command_bytewise(
    ssh_client_channel: &mut Channel<russh::client::Msg>,
    command: &[u8],
    phase: &'static str,
) -> Result<(), ConsoleActivateError> {
    for byte in command {
        ssh_client_channel
            .data([*byte].as_slice())
            .await
            .map_err(|error| ConsoleActivateError::Request { phase, error })?;
    }
    ssh_client_channel
        .data(b"\n".as_slice())
        .await
        .map_err(|error| ConsoleActivateError::Request {
            phase: "sending data to BMC",
            error,
        })?;
    Ok(())
}

/// Returns `true` if `buf` contains the byte sequence `pat` anywhere
/// (contiguously), running in O(n*m) time (n = buf.len(), m = pat.len())
/// and doing no heap allocations.
fn ringbuf_contains<T, RB>(buf: &RB, pat: &[T]) -> bool
where
    RB: ringbuf::consumer::Consumer<Item = T>,
    T: std::cmp::PartialEq,
{
    let pat_len = pat.len();

    // Empty pattern always matches
    if pat_len == 0 {
        return true;
    }
    // If pattern is longer than buffer, can't match
    if pat_len > buf.occupied_len() {
        return false;
    }

    // Get the two contiguous slices that back the ring buffer
    let (s1, s2) = buf.as_slices();

    // 1) Search wholly inside the first slice
    if s1.windows(pat_len).any(|w| w == pat) {
        return true;
    }
    // 2) Search wholly inside the second slice
    if s2.windows(pat_len).any(|w| w == pat) {
        return true;
    }

    // 3) Search across the wrap-around boundary:
    //    for each split k (1..pat_len-1),
    //    check last k bytes of s1 == pat[..k]
    //    and first pat_len-k bytes of s2 == pat[k..]
    let s1_len = s1.len();
    let s2_len = s2.len();
    for k in 1..pat_len {
        if k <= s1_len
            && pat_len - k <= s2_len
            && s1[s1_len - k..] == pat[..k]
            && s2[..pat_len - k] == pat[k..]
        {
            return true;
        }
    }

    false
}

#[test]
fn test_ringbuf_contains() {
    let mut rb = LocalRb::new(6);
    rb.push_slice_overwrite(b"rustacean");
    // buffer holds "tacean" (last 6 of "rustacean")

    assert!(ringbuf_contains(&rb, b"ace"));
    assert!(ringbuf_contains(&rb, b"cean"));
    assert!(ringbuf_contains(&rb, b"tacean"));
    assert!(!ringbuf_contains(&rb, b"rust"));
    assert!(ringbuf_contains(&rb, b"")); // empty always true
    assert!(!ringbuf_contains(&rb, b"rustacean")); // longer than buf
    assert!(!ringbuf_contains(&rb, b"aean")); // non-contiguous
}

#[test]
fn output_after_last_command_uses_final_command_echo() {
    assert_eq!(
        output_after_last_command(
            b"console kill 1\r\nsystem> console 1\r\n",
            b"console kill 1\nconsole 1",
        ),
        Some(b"console 1\r\n".as_slice()),
    );
    assert_eq!(
        output_after_last_command(
            b"stale console start\r\nsystem> console start\r\n",
            b"console start",
        ),
        Some(b"console start\r\n".as_slice()),
    );
    assert_eq!(
        output_after_last_command(
            b"console kill 1\r\nThe command line contains extraneous arguments\r\n",
            b"console kill 1\nconsole 1",
        ),
        None,
    );
}

#[derive(Clone)]
pub(in crate::bmc) struct ConnectionDetails {
    pub(in crate::bmc) machine_id: MachineId,
    pub(in crate::bmc) addr: SocketAddr,
    pub(in crate::bmc) user: String,
    pub(in crate::bmc) password: String,
    pub(in crate::bmc) ssh_key_path: Option<PathBuf>,
    pub(in crate::bmc) bmc_vendor: SshBmcVendor,
}

impl Debug for ConnectionDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Skip writing the password
        f.debug_struct("SshConnectionDetails")
            .field("addr", &self.addr)
            .field("user", &self.user)
            .field("ssh_key_path", &self.ssh_key_path)
            .field("bmc_vendor", &self.bmc_vendor)
            .field("machine_id", &self.machine_id.to_string())
            .finish()
    }
}
