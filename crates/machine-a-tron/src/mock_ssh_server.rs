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
use std::time::Duration;

use bmc_mock::HostnameQuerying;
use eyre::Context;
use rand::rand_core::UnwrapErr;
use rand::rngs::SysRng;
use russh::keys::PublicKeyBase64;
use russh::server::{Auth, ChannelOpenHandle, Config, Msg, Server as _, Session, run_stream};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Pty, server};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

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

pub async fn spawn(
    port: Option<u16>,
    prompt_hostname: Arc<dyn HostnameQuerying>,
    require_credentials: Option<Credentials>,
    prompt_behavior: PromptBehavior,
) -> eyre::Result<MockSshServerHandle> {
    let mut rng = SysRng;
    let host_key =
        russh::keys::PrivateKey::random(&mut UnwrapErr(&mut rng), russh::keys::Algorithm::Ed25519)?;
    let host_pubkey = host_key.public_key_base64();
    let server = Server {
        prompt_hostname,
        prompt_behavior,
        require_credentials,
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
        )
    }
}

struct MockSshHandler {
    prompt_hostname: Arc<dyn HostnameQuerying>,
    prompt_behavior: PromptBehavior,
    console_state: ConsoleState,
    buffer: Vec<u8>,
    require_credentials: Option<Credentials>,
}

impl MockSshHandler {
    fn new(
        prompt_hostname: Arc<dyn HostnameQuerying>,
        prompt_behavior: PromptBehavior,
        require_credentials: Option<Credentials>,
    ) -> Self {
        Self {
            prompt_hostname,
            prompt_behavior,
            console_state: ConsoleState::default(),
            buffer: Vec::default(),
            require_credentials,
        }
    }

    fn print_prompt(
        &self,
        session: &mut Session,
        channel: ChannelId,
    ) -> StdResult<(), russh::Error> {
        match self.console_state {
            ConsoleState::SystemConsole => {
                session.data(
                    channel,
                    format!("\r\nroot@{} # ", self.prompt_hostname.get_hostname()),
                )?;
            }
            ConsoleState::Bmc => match self.prompt_behavior {
                PromptBehavior::LenovoSr650 => session.data(channel, "\nsystem>")?,
                PromptBehavior::Hpe => session.data(channel, "\n</>hpiLO->")?,
                PromptBehavior::Dell => session.data(channel, "\nracadm>>")?,
                PromptBehavior::Dpu | PromptBehavior::LenovoAmi => {}
            },
            ConsoleState::NoShell => {
                // Do nothing
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default, Copy, Clone)]
enum ConsoleState {
    #[default]
    NoShell,
    Bmc,
    SystemConsole,
}

impl server::Handler for MockSshHandler {
    type Error = russh::Error;

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> StdResult<(), Self::Error> {
        tracing::debug!("channel_open_session");
        reply.accept().await;
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
                self.console_state = ConsoleState::Bmc;
            }
            PromptBehavior::Dpu | PromptBehavior::LenovoAmi => {
                self.console_state = ConsoleState::SystemConsole;
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
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
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
                    let print_prompt = match self.prompt_behavior {
                        PromptBehavior::Dell if command.starts_with(b"connect com2") => {
                            tracing::info!(
                                "Got `connect com2` in bmc prompt, simulating system console"
                            );
                            self.console_state = ConsoleState::SystemConsole;
                            true
                        }
                        PromptBehavior::LenovoSr650 if command.starts_with(b"console kill 1") => {
                            tracing::info!(
                                "Got unsupported Lenovo `console kill 1`, simulating BMC error"
                            );
                            session.data(
                                channel,
                                "\r\nThe command line contains extraneous arguments\r\n",
                            )?;
                            // Model the fragmented XCC3 response seen in production: the error
                            // text and trailing BMC prompt arrive in separate SSH packets.
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            true
                        }
                        PromptBehavior::LenovoSr650 if command.starts_with(b"console kill") => {
                            tracing::info!(
                                "Got Lenovo `console kill`, simulating terminated SOL session"
                            );
                            session.data(channel, "\r\nSession on channel 1 is terminated\r\n")?;
                            true
                        }
                        PromptBehavior::LenovoSr650 if command.starts_with(b"console start") => {
                            tracing::info!(
                                "Got Lenovo `console start`, simulating silent system console"
                            );
                            self.console_state = ConsoleState::SystemConsole;
                            // XCC3 emits no success message after `console start`. The host prompt
                            // appears only after the connected frontend sends input.
                            false
                        }
                        PromptBehavior::Hpe if command.starts_with(b"vsp") => {
                            tracing::info!("Got HPE `vsp`, simulating system console");
                            self.console_state = ConsoleState::SystemConsole;
                            true
                        }
                        _ => true,
                    };
                    if print_prompt {
                        self.print_prompt(session, channel)?;
                    }
                } else {
                    self.buffer = [&self.buffer, data].concat();
                    session.data(channel, data.to_owned())?;
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
                        self.console_state = ConsoleState::Bmc;
                    }
                    self.print_prompt(session, channel)?;
                } else {
                    match (data, self.prompt_behavior) {
                        (b"\x1c", PromptBehavior::Dell) => {
                            // ssh-console should have prevented this, make it a warning.
                            tracing::warn!(
                                console_state = ?self.console_state,
                                "Got ctrl+\\ in system console, dropping to BMC prompt",
                            );
                            // ctrl+\
                            self.console_state = ConsoleState::Bmc;
                        }
                        (data, _) => {
                            self.buffer = [&self.buffer, data].concat();
                            session.data(channel, data.to_owned())?;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
