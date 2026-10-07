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

use std::net::SocketAddr;
use std::ops::Add;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eyre::Context;
use russh::ChannelMsg;
use russh::keys::{PrivateKeyWithHashAlg, PublicKey};
use ssh_console::POWER_RESET_COMMAND;
use tokio::sync::oneshot;

// The BMC prompt we get from mock_ssh_server (we shouldn't see this when SSH'ing in.)
static BMC_PROMPT: &[u8] = b"racadm>>";
// How long to wait to see the "normal" prompt (`<host_id>@localhost # `)
static PROMPT_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
// A sequence we can send to mock_ssh_server to simulate the serial console crashing/disconnecting
// and dropping us back to the BMC (to make sure we don't get a BMC prompt.)
static BMC_BACKDOOR_SEQUENCE: &[u8] = b"backdoor_escape_console\n";

#[derive(Copy, Clone)]
pub(crate) struct ConnectionConfig<'a> {
    pub(crate) connection_name: &'a str,
    pub(crate) user: &'a str,
    pub(crate) private_key_path: &'a Path,
    pub(crate) addr: SocketAddr,
    pub(crate) expected_prompt: &'a [u8],
}

pub(crate) async fn assert_connection_works_with_retries_and_timeout(
    connection_config: &ConnectionConfig<'_>,
    retry_count: u8,
    per_try_timeout: Duration,
) -> eyre::Result<()> {
    let mut retries = retry_count;
    loop {
        match tokio::time::timeout(per_try_timeout, assert_connection_works(connection_config))
            .await
        {
            // Didn't timeout, no error
            Ok(Ok(())) => return Ok(()),
            // Didn't timeout, returned error
            Ok(Err(error)) => {
                tracing::error!(
                    ?error,
                    connection_name = connection_config.connection_name,
                    "Error asserting working connection, will retry",
                );
                if retries > 0 {
                    retries -= 1;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                } else {
                    return Err(error).context(format!(
                        "could not connect to {} after {} retries",
                        connection_config.connection_name, retry_count,
                    ));
                }
            }
            // Timed out
            Err(elapsed) => {
                return Err(elapsed).context(format!(
                    "timed out asserting working connection to {}",
                    connection_config.connection_name
                ));
            }
        }
    }
}

pub(crate) async fn assert_connection_works(
    ConnectionConfig {
        connection_name,
        user,
        private_key_path,
        addr,
        expected_prompt,
    }: &ConnectionConfig<'_>,
) -> eyre::Result<()> {
    // Connect to the server and authenticate
    let session = {
        let mut session = russh::client::connect(
            Arc::new(russh::client::Config {
                ..Default::default()
            }),
            addr,
            PermissiveSshClient,
        )
        .await?;

        session
            .authenticate_publickey(
                *user,
                PrivateKeyWithHashAlg::new(
                    Arc::new(
                        russh::keys::load_secret_key(private_key_path, None)
                            .context("error loading ssh private key")?,
                    ),
                    None,
                ),
            )
            .await
            .context("error authenticating with public key")?;

        Ok::<_, eyre::Error>(session)
    }?;

    // Open a session channel
    let mut channel = session
        .channel_open_session()
        .await
        .context("error opening session")?;

    // Request PTY
    channel
        .request_pty(false, "xterm", 80, 24, 0, 0, &[])
        .await
        .context("error requesting PTY")?;

    // Request Shell
    channel.request_shell(false).await?;

    let mut output_buf: Vec<u8> = Vec::new();
    let mut test_state = ConnectionTestState::WaitingForPrompt;
    let prompt_timeout = Instant::now().add(PROMPT_WAIT_TIMEOUT);
    let mut assertion_timeout = Instant::now().add(Duration::from_secs(1));
    let mut write_interval = tokio::time::interval(Duration::from_millis(100));

    // Phase 1: Every second, write a newline to the connection, until we see a prompt, then every
    // 100ms, try to break out with ctrl+\. If 3 seconds go by and we got a prompt but didn't break
    // out, move to phase 2.
    //
    // Phase 2: send `backdoor_escape_console` to the server, which mock_ssh_server will use to
    // simulate the serial console getting disconnected and dropping back down to the BMC prompt. At
    // this point, we expect to be disconnected.
    let result = loop {
        tokio::select! {
            _ = tokio::time::sleep_until(prompt_timeout.into()) => {
                if let ConnectionTestState::WaitingForPrompt = test_state {
                    break Err(eyre::format_err!("Did not see prompt after {PROMPT_WAIT_TIMEOUT:?}"));
                }
            }
            _ = tokio::time::sleep_until(assertion_timeout.into()) => {
                match test_state {
                    ConnectionTestState::TryingCtrlBackslash => {
                        tracing::info!("Successfully prevented ctrl+\\ from triggering escape, now simulating dropping to BMC prompt from other means");
                        test_state = ConnectionTestState::TryingBackdoorEscape;
                        assertion_timeout = Instant::now().add(Duration::from_secs(3));
                    }
                    ConnectionTestState::TryingBackdoorEscape => {
                        tracing::info!("Test finished without seeing a bmc_prompt while using backdoor escape, success");
                        break Ok(());
                    }
                    _ => {}
                }
            }
            _ = write_interval.tick() => {
                match test_state {
                    ConnectionTestState::WaitingForPrompt => {
                        tracing::debug!("Writing newline to server");
                        channel.data(b"\n".as_slice()).await.context("Writing newline to server")?;
                    }
                    ConnectionTestState::TryingCtrlBackslash => {
                        tracing::debug!("Writing ctrl-\\ to server");
                        channel.data(b"\x1c".as_slice()).await.context("Writing ctrl-\\ to server")?;
                    }
                    ConnectionTestState::TryingBackdoorEscape => {
                        tracing::debug!("Writing backdoor escape sequence to server");
                        for byte in BMC_BACKDOOR_SEQUENCE {
                            channel.data([*byte].as_slice()).await.context("Writing mock backdoor escape to server")?;
                        }
                    }
                }
            }
            result = channel.wait() => match result {
                Some(msg) => match msg {
                    ChannelMsg::Data { data } => {
                        output_buf.extend_from_slice(&data);
                        match test_state {
                            ConnectionTestState::WaitingForPrompt => {
                                if output_buf.windows(expected_prompt.len()).any(|w| w == *expected_prompt) {
                                    tracing::info!("Got expected prompt, trying ctrl-\\");
                                    test_state = ConnectionTestState::TryingCtrlBackslash;
                                }
                            }
                            ConnectionTestState::TryingCtrlBackslash | ConnectionTestState::TryingBackdoorEscape => {
                                // We should not generally get any data after sending ctrl-\, since
                                // it should be trapped by ssh-console and not forwarded to the BMC.
                                if output_buf.windows(BMC_PROMPT.len()).any(|window| window == BMC_PROMPT) {
                                    break Err(eyre::format_err!("We escaped to the BMC prompt, this should have been prevented"));
                                }
                            }
                        }
                    }
                    ChannelMsg::Eof => {
                        if matches!(test_state, ConnectionTestState::TryingBackdoorEscape) {
                            tracing::info!(connection_name, "Server sent EOF, all done");
                            break Ok(());
                        } else {
                            break Err(eyre::format_err!("Got disconnected when we weren't expecting it, test_state={test_state:?}"));
                        }
                    }
                    ChannelMsg::WindowAdjusted { .. } => {}
                    _ => {
                        // For now, just error out on unexpected messages, to spot issues sooner. If
                        // this becomes not worth it we can just log and move on.
                        break Err(eyre::format_err!(format!("Unexpected message from server: {:?}", msg)));
                    }
                }
                None => {
                    break Err(eyre::format_err!("Unexpected end of SSH channel"));
                }
            }
        }
    };

    channel.eof().await.ok();
    channel.close().await.ok();

    if result.is_ok() && matches!(test_state, ConnectionTestState::WaitingForPrompt) {
        return Err(eyre::format_err!(format!(
            "did not detect a prompt after connecting to {connection_name}"
        )));
    }

    result
}

pub(crate) async fn fill_logs(
    ConnectionConfig {
        connection_name: _,
        user,
        private_key_path,
        addr,
        expected_prompt: _,
    }: &ConnectionConfig<'_>,
    bytes: usize,
) -> eyre::Result<()> {
    // Connect to the server and authenticate
    let session = {
        let mut session = russh::client::connect(
            Arc::new(russh::client::Config {
                ..Default::default()
            }),
            addr,
            PermissiveSshClient,
        )
        .await?;

        session
            .authenticate_publickey(
                *user,
                PrivateKeyWithHashAlg::new(
                    Arc::new(
                        russh::keys::load_secret_key(private_key_path, None)
                            .context("error loading ssh private key")?,
                    ),
                    None,
                ),
            )
            .await
            .context("error authenticating with public key")?;

        Ok::<_, eyre::Error>(session)
    }?;

    // Open a session channel
    let channel = session
        .channel_open_session()
        .await
        .context("error opening session")?;

    // Request PTY
    channel
        .request_pty(false, "xterm", 80, 24, 0, 0, &[])
        .await
        .context("error requesting PTY")?;

    // Request Shell
    channel.request_shell(false).await?;
    let (mut channel_rx, channel_tx) = channel.split();

    let (done_tx, mut done_rx) = oneshot::channel::<()>();

    // Read until we've seen `bytes` bytes
    tokio::spawn(async move {
        let mut bytes_read = 0;
        while let Some(msg) = channel_rx.wait().await {
            if let ChannelMsg::Data { data } = msg {
                bytes_read += data.len();
                if bytes_read >= bytes {
                    break;
                }
            }
        }
        done_tx.send(()).ok();
    });

    // Write until we're done reading
    let mut write_interval = tokio::time::interval(Duration::from_millis(1));
    loop {
        tokio::select! {
            _ = write_interval.tick() => {
                channel_tx
                    .data(b"fakedatafakedatafakedata\n".as_slice())
                    .await
                    .context("error writing data")?;
            }
            _ = &mut done_rx => break,
        }
    }

    channel_tx.eof().await.ok();
    channel_tx.close().await.ok();

    Ok(())
}

pub(crate) async fn assert_reboot_behavior(
    ConnectionConfig {
        connection_name: _,
        user,
        private_key_path,
        addr,
        expected_prompt: _,
    }: &ConnectionConfig<'_>,
    supported: bool,
) -> eyre::Result<()> {
    // Connect to the server and authenticate
    let session = {
        let mut session = russh::client::connect(
            Arc::new(russh::client::Config {
                ..Default::default()
            }),
            addr,
            PermissiveSshClient,
        )
        .await?;

        session
            .authenticate_publickey(
                *user,
                PrivateKeyWithHashAlg::new(
                    Arc::new(
                        russh::keys::load_secret_key(private_key_path, None)
                            .context("error loading ssh private key")?,
                    ),
                    None,
                ),
            )
            .await
            .context("error authenticating with public key")?;

        Ok::<_, eyre::Error>(session)
    }?;

    // Open a session channel
    let mut channel = session
        .channel_open_session()
        .await
        .context("error opening session")?;

    // Issue reboot command
    channel.exec(true, POWER_RESET_COMMAND).await?;
    let mut exit_status = None;
    let mut buf = Vec::new();
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { data } => {
                buf.extend_from_slice(&data);
            }
            ChannelMsg::ExitStatus { exit_status: e } => {
                exit_status = Some(e);
            }
            _ => {}
        }
    }

    let Some(exit_status) = exit_status else {
        return Err(eyre::format_err!(
            "sending reboot command did not return an exit status"
        ));
    };

    let output = String::from_utf8_lossy(&buf);
    let has_successful_response = output.contains("Power reset completed successfully");

    if supported {
        if !has_successful_response {
            return Err(eyre::format_err!(
                "sending reboot command did not return expected output. output={output}, exit_status={exit_status}"
            ));
        }
        if exit_status != 0 {
            return Err(eyre::format_err!(
                "sending reboot command returned a nonzero exit status. output={output}, exit_status={exit_status}"
            ));
        }
    } else {
        if has_successful_response {
            return Err(eyre::format_err!(
                "sending reboot command returned successful output, but was not supposed to. output={output}, exit_status={exit_status}"
            ));
        }
        if exit_status == 0 {
            return Err(eyre::format_err!(
                "sending reboot command returned successful exit status, but was not supposed to. output={output}, exit_status={exit_status}"
            ));
        }
    }

    Ok(())
}

#[derive(Debug)]
enum ConnectionTestState {
    WaitingForPrompt,
    TryingCtrlBackslash,
    // mock_ssh_server lets the string "backdoor_escape_console" cause the simulated console to exit
    // and drop back to the BMC prompt, so that we can test the code which checks for this.
    TryingBackdoorEscape,
}

pub(crate) struct PermissiveSshClient;

impl russh::client::Handler for PermissiveSshClient {
    type Error = eyre::Error;
    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}
