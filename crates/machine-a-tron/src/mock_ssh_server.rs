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
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::result::Result as StdResult;
use std::sync::Arc;

use bmc_mock::HostnameQuerying;
use bytes::Bytes;
use eyre::Context;
use futures::StreamExt;
use rand::rand_core::UnwrapErr;
use rand::rngs::SysRng;
use russh::keys::PublicKeyBase64;
use russh::server::{Auth, ChannelOpenHandle, Config, Msg, Server as _, Session, run_stream};
use russh::{Channel, ChannelId, ChannelWriteHalf, MethodKind, MethodSet, Pty, server};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::console_output::ConsoleOutputController;

const INTERACTIVE_OUTPUT_CAPACITY: usize = 32;

#[derive(Debug)]
pub struct MockSshServerHandle {
    pub host_pubkey: String,
    pub port: u16,
    _shutdown_handle: Option<oneshot::Sender<()>>,
}

#[derive(Debug, Clone)]
pub struct Credentials {
    pub user: String,
    pub password: String,
}

#[derive(Copy, Clone)]
pub enum PromptBehavior {
    /// Dell iDRAC starts at its BMC prompt and enters the host console with `connect com2`.
    Dell,
    /// A DPU SSH connection opens directly into its system console.
    Dpu,
    /// Lenovo XClarity starts at its BMC prompt and enters the host console with `console start`.
    LenovoSr650,
    /// A Lenovo AMI SSH connection opens directly into its system console.
    LenovoAmi,
    /// HPE iLO starts at its BMC prompt and enters the host console with `vsp`.
    Hpe,
}

/// Starts a mock BMC SSH listener with optional machine-owned system-console output.
///
/// Generated output is visible only after the session enters system-console mode. Interactive
/// echoes and prompts use a bounded per-session writer queue and are never shed.
pub async fn spawn(
    port: Option<u16>,
    prompt_hostname: Arc<dyn HostnameQuerying>,
    require_credentials: Option<Credentials>,
    prompt_behavior: PromptBehavior,
    console_output: Option<ConsoleOutputController>,
) -> eyre::Result<MockSshServerHandle> {
    let mut rng = SysRng;
    let host_key =
        russh::keys::PrivateKey::random(&mut UnwrapErr(&mut rng), russh::keys::Algorithm::Ed25519)?;
    let host_pubkey = host_key.public_key_base64();
    let server = Server {
        prompt_hostname,
        prompt_behavior,
        require_credentials,
        console_output,
    };
    let listener = if let Some(port) = port {
        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
        TcpListener::bind(socket_addr)
            .await
            .context(format!("error listening on {socket_addr}"))?
    } else {
        TcpListener::bind("0.0.0.0:0")
            .await
            .context("error listening on 0.0.0.0:0")?
    };

    let port = listener.local_addr()?.port();

    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(server.run(
        Arc::new(russh::server::Config {
            keys: vec![host_key],
            ..Default::default()
        }),
        listener,
        rx,
    ));

    Ok(MockSshServerHandle {
        _shutdown_handle: Some(tx),
        port,
        host_pubkey,
    })
}

#[derive(Clone)]
struct Server {
    prompt_hostname: Arc<dyn HostnameQuerying>,
    prompt_behavior: PromptBehavior,
    require_credentials: Option<Credentials>,
    console_output: Option<ConsoleOutputController>,
}

impl Server {
    async fn run(
        mut self,
        config: Arc<Config>,
        socket: TcpListener,
        mut shutdown: oneshot::Receiver<()>,
    ) -> eyre::Result<()> {
        loop {
            tokio::select! {
                accept_result = socket.accept() => {
                    match accept_result {
                        Ok((socket, _)) => {
                            let config = config.clone();
                            let handler = self.new_client(socket.peer_addr().ok());

                            tokio::spawn(async move {
                                if config.nodelay
                                    && let Err(e) = socket.set_nodelay(true) {
                                        tracing::warn!(
                                            error = ?e,
                                            "set_nodelay failed",
                                        );
                                    }

                                let session = match run_stream(config, socket, handler).await {
                                    Ok(s) => s,
                                    Err(error) => {
                                        if !matches!(error, russh::Error::Disconnect) {
                                            tracing::warn!(?error, "Connection setup failed");
                                        }
                                        return
                                    }
                                };

                                match session.await {
                                    Ok(_) => tracing::debug!("Connection closed"),
                                    Err(russh::Error::Disconnect) => {},
                                    Err(error) => {
                                        tracing::warn!(?error, "Connection closed with error");
                                    }
                                }
                            });
                        }

                        Err(error) => {
                            tracing::error!(?error, "Error accepting SSH connection from socket");
                            break;
                        },
                    }
                },

                _ = &mut shutdown => break,
            }
        }

        Ok(())
    }
}

impl server::Server for Server {
    type Handler = MockSshHandler;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self::Handler {
        MockSshHandler::new(
            self.prompt_hostname.clone(),
            self.prompt_behavior,
            self.require_credentials.clone(),
            self.console_output.clone(),
        )
    }
}

struct MockSshHandler {
    prompt_hostname: Arc<dyn HostnameQuerying>,
    prompt_behavior: PromptBehavior,
    console_state: ConsoleState,
    buffer: Vec<u8>,
    require_credentials: Option<Credentials>,
    console_output: Option<ConsoleOutputController>,
    console_state_tx: watch::Sender<ConsoleState>,
    interactive_output: Option<mpsc::Sender<Bytes>>,
    writer_task: Option<JoinHandle<()>>,
}

impl MockSshHandler {
    fn new(
        prompt_hostname: Arc<dyn HostnameQuerying>,
        prompt_behavior: PromptBehavior,
        require_credentials: Option<Credentials>,
        console_output: Option<ConsoleOutputController>,
    ) -> Self {
        let (console_state_tx, _) = watch::channel(ConsoleState::default());
        Self {
            prompt_hostname,
            prompt_behavior,
            console_state: ConsoleState::default(),
            buffer: Vec::default(),
            require_credentials,
            console_output,
            console_state_tx,
            interactive_output: None,
            writer_task: None,
        }
    }

    async fn print_prompt(&self) -> StdResult<(), russh::Error> {
        match self.console_state {
            ConsoleState::SystemConsole => {
                self.send_interactive(format!(
                    "\r\nroot@{} # ",
                    self.prompt_hostname.get_hostname()
                ))
                .await?;
            }
            ConsoleState::Bmc => match self.prompt_behavior {
                PromptBehavior::LenovoSr650 => self.send_interactive("\nsystem>").await?,
                PromptBehavior::Hpe => self.send_interactive("\n</>hpiLO->").await?,
                PromptBehavior::Dell => self.send_interactive("\nracadm>>").await?,
                PromptBehavior::Dpu | PromptBehavior::LenovoAmi => {}
            },
            ConsoleState::NoShell => {
                // Do nothing
            }
        }
        Ok(())
    }

    fn set_console_state(&mut self, console_state: ConsoleState) {
        self.console_state = console_state;
        self.console_state_tx.send_replace(console_state);
    }

    async fn send_interactive(&self, data: impl Into<Bytes>) -> StdResult<(), russh::Error> {
        let Some(output) = &self.interactive_output else {
            return Err(russh::Error::Disconnect);
        };
        output
            .send(data.into())
            .await
            .map_err(|_| russh::Error::Disconnect)
    }
}

impl Drop for MockSshHandler {
    fn drop(&mut self) {
        if let Some(task) = self.writer_task.take() {
            task.abort();
        }
    }
}

#[derive(Debug, Default, Copy, Clone, Eq, PartialEq)]
enum ConsoleState {
    #[default]
    NoShell,
    Bmc,
    SystemConsole,
}

async fn run_console_writer(
    output: ChannelWriteHalf<Msg>,
    mut interactive: mpsc::Receiver<Bytes>,
    mut console_state: watch::Receiver<ConsoleState>,
    console_output: Option<ConsoleOutputController>,
) {
    let mut console_output = console_output.map(|controller| controller.stream());
    loop {
        let write_result = tokio::select! {
            biased;
            interactive = interactive.recv() => match interactive {
                Some(bytes) => output.data_bytes(bytes).await,
                None => return,
            },
            changed = console_state.changed() => {
                if changed.is_err() {
                    return;
                }
                continue;
            },
            next = async {
                console_output
                    .as_mut()
                    .expect("branch guard requires console output")
                    .next()
                    .await
            }, if console_output.is_some()
                && *console_state.borrow() == ConsoleState::SystemConsole =>
            {
                match next {
                    Some(bytes) => output.data_bytes(bytes).await,
                    None => {
                        console_output = None;
                        continue;
                    }
                }
            }
        };

        if let Err(error) = write_result {
            tracing::debug!(%error, "mock SSH console writer stopped");
            return;
        }
    }
}

impl server::Handler for MockSshHandler {
    type Error = russh::Error;

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> StdResult<(), Self::Error> {
        tracing::debug!("channel_open_session");
        reply.accept().await;
        let (_, output) = channel.split();
        let (interactive_output, interactive) = mpsc::channel(INTERACTIVE_OUTPUT_CAPACITY);
        self.interactive_output = Some(interactive_output);
        self.writer_task = Some(tokio::spawn(run_console_writer(
            output,
            interactive,
            self.console_state_tx.subscribe(),
            self.console_output.clone(),
        )));
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> StdResult<(), Self::Error> {
        tracing::debug!("pty_request");
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> StdResult<(), Self::Error> {
        tracing::debug!("shell_request");
        match self.prompt_behavior {
            PromptBehavior::Dell | PromptBehavior::LenovoSr650 | PromptBehavior::Hpe => {
                self.set_console_state(ConsoleState::Bmc);
            }
            PromptBehavior::Dpu | PromptBehavior::LenovoAmi => {
                self.set_console_state(ConsoleState::SystemConsole);
            }
        }
        session.channel_success(channel)?;
        Ok(())
    }

    async fn auth_none(&mut self, _user: &str) -> StdResult<Auth, Self::Error> {
        Ok(server::Auth::Reject {
            proceed_with_methods: Some(MethodSet::from([MethodKind::Password].as_slice())),
            partial_success: false,
        })
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> StdResult<Auth, Self::Error> {
        if let Some(require_credentials) = &self.require_credentials {
            if user == require_credentials.user && password == require_credentials.password {
                tracing::info!("got correct auth_password, accepting");
                Ok(server::Auth::Accept)
            } else {
                tracing::info!(
                    user = %user,
                    "Incorrect SSH password; rejecting authentication",
                );
                Ok(server::Auth::Reject {
                    proceed_with_methods: None,
                    partial_success: false,
                })
            }
        } else {
            tracing::info!(
                user = %user,
                "Accepting SSH credentials because any credentials are allowed",
            );
            Ok(server::Auth::Accept)
        }
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> StdResult<(), Self::Error> {
        // Sending Ctrl+C ends the session and disconnects the client
        if data == [3] {
            return Err(russh::Error::Disconnect);
        }

        match self.console_state {
            ConsoleState::NoShell => {
                tracing::warn!("data sent without shell request");
            }
            ConsoleState::Bmc => {
                if data == b"\n" || data == b"\r\n" || data == b"\r" {
                    let command = std::mem::take(&mut self.buffer);
                    match self.prompt_behavior {
                        PromptBehavior::Dell if command.starts_with(b"connect com2") => {
                            tracing::info!(
                                "Got `connect com2` in bmc prompt, simulating system console"
                            );
                            self.set_console_state(ConsoleState::SystemConsole);
                        }
                        PromptBehavior::LenovoSr650 if command.starts_with(b"console kill 1") => {
                            tracing::info!(
                                "Got unsupported Lenovo `console kill 1`, simulating BMC error"
                            );
                            self.send_interactive(
                                "\r\nThe command line contains extraneous arguments\r\n",
                            )
                            .await?;
                        }
                        PromptBehavior::LenovoSr650 if command.starts_with(b"console kill") => {
                            tracing::info!(
                                "Got Lenovo `console kill`, simulating terminated SOL session"
                            );
                            self.send_interactive("\r\nSession on channel 1 is terminated\r\n")
                                .await?;
                        }
                        PromptBehavior::LenovoSr650 if command.starts_with(b"console start") => {
                            tracing::info!("Got Lenovo `console start`, simulating system console");
                            self.set_console_state(ConsoleState::SystemConsole);
                        }
                        PromptBehavior::Hpe if command.starts_with(b"vsp") => {
                            tracing::info!("Got HPE `vsp`, simulating system console");
                            self.set_console_state(ConsoleState::SystemConsole);
                        }
                        _ => {}
                    }
                    self.print_prompt().await?;
                } else {
                    self.buffer = [&self.buffer, data].concat();
                    self.send_interactive(data.to_owned()).await?;
                }
            }
            ConsoleState::SystemConsole => {
                if data == b"\n" || data == b"\r\n" || data == b"\r" {
                    let command = std::mem::take(&mut self.buffer);
                    if matches!(self.prompt_behavior, PromptBehavior::Dell)
                        && command.starts_with(b"backdoor_escape_console")
                    {
                        tracing::info!(
                            "Got backdoor command to simulate escaping console, dropping to BMC prompt"
                        );
                        self.set_console_state(ConsoleState::Bmc);
                    }
                    self.print_prompt().await?;
                } else {
                    match (data, self.prompt_behavior) {
                        (b"\x1c", PromptBehavior::Dell) => {
                            // ssh-console should have prevented this, make it a warning.
                            tracing::warn!(
                                console_state = ?self.console_state,
                                "Got ctrl+\\ in system console, dropping to BMC prompt",
                            );
                            // ctrl+\
                            self.set_console_state(ConsoleState::Bmc);
                        }
                        (data, _) => {
                            self.buffer = [&self.buffer, data].concat();
                            self.send_interactive(data.to_owned()).await?;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
