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

use bytes::Bytes;
use chrono::{DateTime, Utc};
use russh::ChannelMsg;
use russh::server::Msg;
use tokio::sync::{broadcast, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::shutdown_handle::ShutdownHandle;

/// Proxy messages from the BMC to the user's connection.
pub(crate) fn spawn(
    mut from_bmc_rx: broadcast::Receiver<ToFrontendMessage>,
    to_frontend_tx: russh::ChannelWriteHalf<Msg>,
    peer_addr: String,
) -> Handle {
    let (cancel_token, drop_guard) = {
        let t = CancellationToken::new();
        (t.clone(), t.drop_guard())
    };
    let join_handle = tokio::spawn({
        async move {
            while let Some(res) = cancel_token.run_until_cancelled(from_bmc_rx.recv()).await {
                match res {
                    Ok(msg) => {
                        let msg = Arc::<ChannelMsg>::from(msg);
                        match proxy_channel_message(msg.as_ref(), &to_frontend_tx).await {
                            Ok(()) => {}
                            Err(error) => {
                                tracing::debug!(
                                    peer_address = peer_addr,
                                    %error,
                                    "error sending message to frontend, likely disconnected"
                                );
                                break;
                            }
                        }
                    }
                    Err(_) => {
                        tracing::debug!(
                            peer_address = peer_addr,
                            "client channel closed when writing message from BMC"
                        );
                        break;
                    }
                }
            }
            to_frontend_tx.close().await.ok();
        }
    });

    Handle {
        drop_guard,
        join_handle,
    }
}

pub(crate) struct Handle {
    drop_guard: DropGuard,
    join_handle: JoinHandle<()>,
}

impl ShutdownHandle<()> for Handle {
    fn into_parts(self) -> (DropGuard, JoinHandle<()>) {
        (self.drop_guard, self.join_handle)
    }
}

/// Holds messages to be sent to a frontend: Data from the BMC channel, or connection status messages.
#[derive(Clone)]
pub(crate) enum ToFrontendMessage {
    /// Data coming from the BMC
    Channel(Arc<ChannelMsg>),
    /// An alert that the console was connected or disconnected
    ConnectionChanged(ConnectionChangeMessage),
    /// A reply to the user pressing the Enter key when the BMC is disconnected
    InformDisconnectedSince(Option<DateTime<Utc>>),
}

#[derive(Clone)]
pub(crate) enum ConnectionChangeMessage {
    Disconnected,
    Connected {
        last_disconnect: Option<DateTime<Utc>>,
    },
}

impl From<ToFrontendMessage> for Arc<ChannelMsg> {
    fn from(msg: ToFrontendMessage) -> Self {
        match msg {
            ToFrontendMessage::ConnectionChanged(connection_changed) => connection_changed.into(),
            ToFrontendMessage::InformDisconnectedSince(Some(disconnected_since)) => {
                let data: Bytes = format!(
                    "--- Console disconnected since {} ---\r\n",
                    disconnected_since.to_rfc2822()
                )
                .into();
                Arc::new(ChannelMsg::Data { data })
            }
            ToFrontendMessage::InformDisconnectedSince(None) => {
                let data: Bytes = "--- Console not connected ---\r\n".into();
                Arc::new(ChannelMsg::Data { data })
            }
            ToFrontendMessage::Channel(msg) => msg,
        }
    }
}

impl From<ConnectionChangeMessage> for Arc<ChannelMsg> {
    fn from(value: ConnectionChangeMessage) -> Self {
        let data: Bytes = match value {
            ConnectionChangeMessage::Disconnected => "\r\n--- Console disconnected! ---\r\n".into(),
            ConnectionChangeMessage::Connected { last_disconnect } => {
                if let Some(last_disconnect) = last_disconnect {
                    format!(
                        "\r\n--- Console connected! Last disconnect: {} ---\r\n",
                        last_disconnect.to_rfc2822()
                    )
                    .into_bytes()
                    .into()
                } else {
                    b"\r\n--- Console connected! ---\r\n".to_vec().into()
                }
            }
        };

        Arc::new(ChannelMsg::Data { data })
    }
}

/// Take a russh::ChannelMsg being sent in either direction from the frontend or the BMC, and call
/// the appropriate method on the underlying russh channel.
///
/// This is the main proxy logic between the frontend SSH connection and the backend BMC connection.
/// This whole thing would be unnecessary if [`russh::channels::ChanelWriteHalf::send_msg`] were
/// public. :(
pub(super) async fn proxy_channel_message<S>(
    channel_msg: &russh::ChannelMsg,
    channel: &russh::ChannelWriteHalf<S>,
) -> Result<(), MessageProxyError>
where
    S: From<(russh::ChannelId, russh::ChannelMsg)> + Send + Sync + 'static,
{
    use MessageProxyError::*;
    match channel_msg {
        ChannelMsg::Open { .. } => {}
        ChannelMsg::Data { data } => {
            channel
                .data(data.iter().as_slice())
                .await
                .map_err(|error| Sending {
                    what: "data",
                    error,
                })?;
        }
        ChannelMsg::ExtendedData { data, ext } => {
            channel
                .extended_data(*ext, data.iter().as_slice())
                .await
                .map_err(|error| Sending {
                    what: "extended data",
                    error,
                })?;
        }
        ChannelMsg::Eof => {
            channel
                .eof()
                .await
                .map_err(|error| Sending { what: "eof", error })?;
        }
        ChannelMsg::Close => {
            channel.close().await.map_err(|error| Sending {
                what: "close",
                error,
            })?;
        }
        ChannelMsg::RequestPty {
            want_reply,
            term,
            col_width,
            row_height,
            pix_width,
            pix_height,
            terminal_modes,
        } => {
            channel
                .request_pty(
                    *want_reply,
                    term,
                    *col_width,
                    *row_height,
                    *pix_width,
                    *pix_height,
                    terminal_modes,
                )
                .await
                .map_err(|error| Sending {
                    what: "pty request",
                    error,
                })?;
        }
        ChannelMsg::RequestShell { want_reply } => {
            channel
                .request_shell(*want_reply)
                .await
                .map_err(|error| Sending {
                    what: "shell request",
                    error,
                })?;
        }
        ChannelMsg::Signal { signal } => {
            channel
                .signal(signal.clone())
                .await
                .map_err(|error| Sending {
                    what: "signal",
                    error,
                })?;
        }
        ChannelMsg::WindowChange {
            col_width,
            row_height,
            pix_width,
            pix_height,
        } => {
            channel
                .window_change(*col_width, *row_height, *pix_width, *pix_height)
                .await
                .map_err(|error| Sending {
                    what: "window change",
                    error,
                })?;
        }
        _ => {
            tracing::debug!(?channel_msg, "Ignoring unknown channel message");
        }
    }

    Ok(())
}

#[derive(thiserror::Error, Debug)]
pub(super) enum MessageProxyError {
    #[error("error sending {what}: {error}")]
    Sending {
        what: &'static str,
        error: russh::Error,
    },
}

#[derive(Debug)]
pub(crate) enum ToBmcMessage {
    /// Normal SSH message
    ChannelMsg(ChannelMsg),
    /// Exec request (e.g. power reset)
    Exec {
        command: Vec<u8>,
        reply_tx: oneshot::Sender<ExecReply>,
    },
    /// Message scoped to a single client (not seen by other clients or by logging), requesting
    /// information on whether the connection is up, number of bytes received, etc. This is not sent
    /// to the BMC but intercepted by our connection_impl.
    EchoConnectionMessage { reply_tx: oneshot::Sender<Vec<u8>> },
}

#[derive(Debug)]
pub(crate) struct ExecReply {
    pub(crate) output: Vec<u8>,
    pub(crate) exit_status: u32,
}
