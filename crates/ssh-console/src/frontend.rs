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
use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use carbide_uuid::machine::MachineId;
use lazy_static::lazy_static;
use rpc::forge::ValidateTenantPublicKeyRequest;
use rpc::forge_api_client::ForgeApiClient;
use russh::keys::ssh_key::AuthorizedKeys;
use russh::keys::{Certificate, PublicKey, PublicKeyBase64};
use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId, ChannelMsg, MethodKind, MethodSet, Pty};
use tokio::sync::oneshot;
use tonic::Code;
use uuid::Uuid;

use crate::bmc::client::BmcConnectionSubscription;
use crate::bmc::client_pool::{BmcConnectionStore, GetConnectionError};
use crate::bmc::connection::Kind;
use crate::bmc::message_proxy;
use crate::bmc::message_proxy::{ExecReply, ToBmcMessage};
use crate::config::Config;
use crate::shutdown_handle::ShutdownHandle;
use crate::ssh_cert_parsing::{certificate_contains_role, get_user_from_certificate};
use crate::ssh_server::ServerMetrics;

static EXEC_TIMEOUT: Duration = Duration::from_secs(10);

static BANNER_SSH_BMC: &str = "\
+------------------------------------------------------------------------------+\r\n\
|                NVIDIA Carbide SSH Serial Console (beta)                      |\r\n\
+------------------------------------------------------------------------------+\r\n\
|             Use SSH escape sequences to manage this session.                 |\r\n\
|      (Note that escapes are only recognized immediately after newline.)      |\r\n\
|                               ~. | terminate session                         |\r\n\
|                               ~? | Help                                      |\r\n\
+------------------------------------------------------------------------------+\r\n\
";

static BANNER_IPMI_BMC: &str = "\
+------------------------------------------------------------------------------+\r\n\
|                NVIDIA Carbide SSH Serial Console (beta)                      |\r\n\
+------------------------------------------------------------------------------+\r\n\
|             Use SSH escape sequences to manage this session.                 |\r\n\
|      (Note that escapes are only recognized immediately after newline.)      |\r\n\
|                               ~. | terminate session                         |\r\n\
|                               ~? | Help                                      |\r\n\
|   This system supports power reset requests. To reboot this system, append   |\r\n\
|                \"power reset\" to your original SSH command                  |\r\n\
|                (e.g. ssh <host>@<console-ip> power reset)                    |\r\n\
+------------------------------------------------------------------------------+\r\n\
";

lazy_static! {
    static ref CERT_AUTH_FAILURE_METRIC: [opentelemetry::KeyValue; 1] =
        [opentelemetry::KeyValue::new(
            "auth_type",
            "openssh_certificate",
        )];
    static ref PUBKEY_AUTH_FAILURE_METRIC: [opentelemetry::KeyValue; 1] =
        [opentelemetry::KeyValue::new("auth_type", "public_key",)];
}

pub(crate) struct Handler {
    config: Arc<Config>,
    forge_api_client: ForgeApiClient,
    bmc_connection_store: BmcConnectionStore,
    /// The machine_id or instance_id the user is attempting to log into. Used as the username in the ssh command line (ie. ssh machine_id@ssh-console)
    authenticated_machine_string: Option<String>,
    per_client_state: HashMap<ChannelId, PerClientState>,
    metrics: Arc<ServerMetrics>,
    last_auth_failure: Option<AuthFailureReason>,
    // Specifically for logging. A string so that we can use <unknown> if we don't get an address at connection time.
    peer_addr: String,
}

struct PerClientState {
    bmc_connection: BmcConnectionSubscription,
    // Option so that it can be taken with .take() when we get a shell_request or exec_request
    client_channel: Option<Channel<Msg>>,
}

impl Handler {
    pub(crate) fn new(
        bmc_connection_store: BmcConnectionStore,
        config: Arc<Config>,
        forge_api_client: ForgeApiClient,
        metrics: Arc<ServerMetrics>,
        peer_addr: Option<SocketAddr>,
    ) -> Self {
        tracing::debug!("spawning new frontend connection handler");
        Self {
            config,
            forge_api_client,
            bmc_connection_store,
            authenticated_machine_string: None,
            per_client_state: HashMap::new(),
            metrics,
            last_auth_failure: Default::default(),
            peer_addr: peer_addr
                .map(|addr| addr.to_string())
                .unwrap_or_else(|| "<unknown>".to_string()),
        }
    }

    fn get_client_state_or_report_error(
        &mut self,
        session: &mut Session,
        channel_id: ChannelId,
    ) -> Option<&mut PerClientState> {
        if let Some(state) = self.per_client_state.get_mut(&channel_id) {
            return Some(state);
        }

        tracing::error!(peer_address = self.peer_addr, "Request on unknown channel");
        session.channel_failure(channel_id).ok();
        session
            .data(channel_id, "ssh-console error: Unknown channel\n")
            .ok();
        session.close(channel_id).ok();
        None
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        tracing::info!(peer_address = self.peer_addr, "end frontend connection");
        // All auth failure paths set self.last_auth_failure, but auth can still succeed (they may
        // be trying multiple pubkeys, etc.) So if authenticated_user is None but last_auth_failure
        // is Some, bump the metrics.
        if let (None, Some(last_auth_failure)) =
            (&self.authenticated_machine_string, &self.last_auth_failure)
        {
            let machine = last_auth_failure.machine_string();
            if let Some(user) = &last_auth_failure.detected_user() {
                tracing::warn!(
                    peer_address = self.peer_addr,
                    %user,
                    %machine,
                    "authentication failed",
                );
            } else {
                tracing::warn!(
                    peer_address = self.peer_addr,
                    %machine,
                    "authentication failed",
                );
            }
            self.metrics
                .client_auth_failures_total
                .add(1, last_auth_failure.metric());
        }
    }
}

impl russh::server::Handler for Handler {
    type Error = HandlerError;

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        use HandlerError::*;
        tracing::trace!(peer_address = self.peer_addr, "channel_open_session");
        let Some(machine) = &self.authenticated_machine_string else {
            return Err(MissingAuthenticatedUser {
                method: "channel_open_session",
            });
        };

        // fetch the BMC connection
        let channel_id = channel.id();
        let bmc_connection = self
            .bmc_connection_store
            .get_connection(
                machine,
                &self.config,
                &self.forge_api_client,
                self.metrics.clone(),
            )
            .await
            .map_err(|error| GettingBmcConnection {
                machine: machine.to_owned(),
                error,
            })?;

        // Save the BMC and client channel in self, so the Handler methods can find it
        self.per_client_state.insert(
            channel_id,
            PerClientState {
                bmc_connection,
                client_channel: Some(channel),
            },
        );

        session
            .channel_success(channel_id)
            .map_err(|error| Replying {
                method: "channel_open_session",
                what: "success",
                error,
            })?;

        reply.accept().await;
        Ok(())
    }

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        tracing::trace!(peer_address = self.peer_addr, "auth_none");
        Ok(Auth::Reject {
            // Note: openssh_certificate auth is just another kind of PublicKey auth, this should
            // imply either one.
            proceed_with_methods: Some(MethodSet::from([MethodKind::PublicKey].as_slice())),
            partial_success: false,
        })
    }

    async fn auth_openssh_certificate(
        &mut self,
        machine_string: &str,
        certificate: &Certificate,
    ) -> Result<Auth, Self::Error> {
        tracing::trace!(peer_address = self.peer_addr, "auth_openssh_certificate");
        let Some(admin_certificate_role) = self.config.admin_certificate_role.as_ref() else {
            tracing::debug!("skipping ssh certificate auth, no admin role is configured");
            return Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            });
        };

        let user =
            get_user_from_certificate(certificate, &self.config.openssh_certificate_authorization)
                .map(str::to_owned);

        let is_trusted = certificate
            .validate(&self.config.openssh_certificate_ca_fingerprints)
            .is_ok();

        if !is_trusted {
            tracing::warn!(
                peer_address = self.peer_addr,
                machine = machine_string,
                "openssh certificate CA certificate not trusted, rejecting authentication"
            );
            self.last_auth_failure = Some(AuthFailureReason::Certificate {
                user,
                machine_string: machine_string.to_owned(),
            });
            return Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            });
        }

        if !certificate_contains_role(
            certificate,
            admin_certificate_role,
            &self.config.openssh_certificate_authorization,
        ) {
            tracing::warn!(
                peer_address = self.peer_addr,
                machine = machine_string,
                role = admin_certificate_role.as_str(),
                "certificate auth failed because the required role is missing",
            );
            self.last_auth_failure = Some(AuthFailureReason::Certificate {
                user,
                machine_string: machine_string.to_owned(),
            });
            return Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            });
        }

        if let Some(user) = &user {
            tracing::info!(
                peer_address = self.peer_addr,
                user = user.as_str(),
                machine = machine_string,
                role = admin_certificate_role.as_str(),
                "certificate auth succeeded"
            );
        } else {
            tracing::info!(
                peer_address = self.peer_addr,
                machine = machine_string,
                role = admin_certificate_role.as_str(),
                "certificate auth succeeded"
            );
        }
        self.authenticated_machine_string = Some(machine_string.to_owned());
        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        machine_string: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        use HandlerError::*;
        tracing::trace!(peer_address = self.peer_addr, "auth_publickey");

        // Authentication flow:
        // 1. If authorized_keys_path is set, check against file first
        // 2. If not found in file, validate via carbide-api
        // 3. If insecure mode is enabled, accept all connections

        let success = if pubkey_auth_admin_authorized_keys(public_key, &self.config, machine_string)
            .map_err(|error| PubkeyAuthAdminAuthorizedKeys {
                machine_id: machine_string.to_owned(),
                error,
            })? {
            true
        } else if Uuid::from_str(machine_string).is_ok() {
            // Only try tenant auth if the user is a valid-looking UUID.
            pubkey_auth_tenant(machine_string, public_key, &self.forge_api_client)
                .await
                .map_err(|error| PubkeyAuthTenant {
                    instance_id: machine_string.to_owned(),
                    error,
                })?
        } else {
            tracing::debug!(
                peer_address = self.peer_addr,
                ssh_username = machine_string,
                "rejecting public key for user"
            );
            false
        };

        let success = if !success && self.config.insecure {
            tracing::info!(
                peer_address = self.peer_addr,
                "Overriding public-key rejection because we are in insecure (testing) mode"
            );
            true
        } else {
            success
        };

        if success {
            self.authenticated_machine_string = Some(machine_string.to_owned());
            Ok(Auth::Accept)
        } else {
            self.last_auth_failure = Some(AuthFailureReason::PubKey {
                machine_string: machine_string.to_owned(),
            });
            Ok(Auth::Reject {
                partial_success: false,
                proceed_with_methods: None,
            })
        }
    }

    /// Forward the data to the BMC
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        tracing::trace!(peer_address = self.peer_addr, "data");
        if let Some(client_state) = self.get_client_state_or_report_error(session, channel) {
            client_state
                .bmc_connection
                .to_bmc_msg_tx
                .send(ToBmcMessage::ChannelMsg(ChannelMsg::Data {
                    data: data.to_vec().into(),
                }))
                .await
                .map_err(|_| HandlerError::WritingToChannel { what: "data" })?;
        }
        Ok(())
    }

    /// Forward the data to the BMC
    async fn extended_data(
        &mut self,
        channel: ChannelId,
        code: u32,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        tracing::trace!(peer_address = self.peer_addr, "extended_data");
        if let Some(client_state) = self.get_client_state_or_report_error(session, channel) {
            client_state
                .bmc_connection
                .to_bmc_msg_tx
                .send(ToBmcMessage::ChannelMsg(ChannelMsg::ExtendedData {
                    data: data.to_vec().into(),
                    ext: code,
                }))
                .await
                .map_err(|_| HandlerError::WritingToChannel {
                    what: "extended_data",
                })?;
        }
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
    ) -> Result<(), Self::Error> {
        tracing::trace!(peer_address = self.peer_addr, "pty_request");
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel_id: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        tracing::trace!(peer_address = self.peer_addr, "shell_request");
        let peer_addr = self.peer_addr.clone();
        let Some(client_state) = self.get_client_state_or_report_error(session, channel_id) else {
            return Ok(());
        };

        // shell requests are when we actually subscribe to the BMC connection. We have to take
        // ownership of the client channel here so that we can drop it when they disconnect, which
        // means we can't support both a shell_request and an exec_request on the same channel
        // (which makes sense.)
        let Some(channel) = client_state.client_channel.take() else {
            tracing::error!(
                peer_address = self.peer_addr,
                "Channel unavailable, cannot service shell request"
            );
            session.channel_failure(channel_id).ok();
            session
                .data(channel_id, "ssh-console error: Channel unavailable\r\n")
                .ok();
            session.close(channel_id).ok();
            return Ok(());
        };
        let machine_id = client_state.bmc_connection.machine_id;
        let Some(from_bmc_rx) = client_state
            .bmc_connection
            .to_frontend_msg_weak_tx
            .upgrade()
            .map(|tx| tx.subscribe())
        else {
            return Err(HandlerError::BmcDisconnectedBeforeSubscribe { machine_id })?;
        };

        // Output the banner with instructions
        let banner = match client_state.bmc_connection.kind {
            Kind::Ssh => BANNER_SSH_BMC.as_bytes(),
            Kind::Ipmi => BANNER_IPMI_BMC.as_bytes(),
        };
        session.data(channel_id, banner).ok();

        // Tell the backend to return any "pending line": data since the last newline
        let (mut channel_rx, channel_tx) = channel.split();
        let (pending_line_reply_tx, pending_line_reply_rx) = oneshot::channel();
        client_state
            .bmc_connection
            .to_bmc_msg_tx
            .send(ToBmcMessage::EchoConnectionMessage {
                reply_tx: pending_line_reply_tx,
            })
            .await
            .ok();
        if let Ok(pending_line) = pending_line_reply_rx.await {
            channel_tx.data(pending_line.as_slice()).await.ok();
        }

        // Proxy messages from the BMC to the user's connection
        // NOTE: We have to go through extra effort to know when to stop proxying messages, because
        // we don't get reliably told when clients disconnect. So we poll for channel_rx here
        // (taking ownership of it) and signal a shutdown of the proxy loop, then when that happens,
        // we finally close the channel. Only then is Self::channel_close() actually sent! (This is
        // IMO a design flaw in russh.)
        let proxy_handle = message_proxy::spawn(from_bmc_rx, channel_tx, peer_addr);

        tokio::spawn({
            async move {
                loop {
                    if channel_rx.wait().await.is_none() {
                        break;
                    }
                }
                proxy_handle.shutdown_and_wait().await;
            }
        });

        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        tracing::trace!(peer_address = self.peer_addr, "exec_request");
        let Some(PerClientState {
            client_channel,
            bmc_connection,
        }) = self.get_client_state_or_report_error(session, channel_id)
        else {
            return Ok(());
        };

        // Drop the client channel when we're done, so that it properly disconnects.
        let Some(channel) = client_channel.take() else {
            tracing::error!(
                peer_address = self.peer_addr,
                "Channel unavailable, cannot service exec request"
            );
            session.channel_failure(channel_id).ok();
            session
                .data(channel_id, "ssh-console error: Channel unavailable\r\n")
                .ok();
            session.close(channel_id).ok();
            return Ok(());
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        bmc_connection
            .to_bmc_msg_tx
            .send(ToBmcMessage::Exec {
                command: data.to_vec(),
                reply_tx,
            })
            .await
            .map_err(|_| HandlerError::WritingToChannel {
                what: "exec request",
            })?;

        tokio::select! {
            _ = tokio::time::sleep(EXEC_TIMEOUT) => {
                    channel
                        .data(b"Error: request timeout\r\n".as_slice())
                        .await
                        .ok();
                    channel.exit_status(1).await.ok();
            }
            res = reply_rx => match res {
                Ok(ExecReply {
                    output,
                    exit_status,
                }) => {
                    channel.data(output.as_slice()).await.ok();
                    channel.exit_status(exit_status).await.ok();
                }
                Err(_) => {
                    channel
                        .data(b"Error: BMC disconnected\r\n".as_slice())
                        .await
                        .ok();
                    channel.exit_status(1).await.ok();
                }
            }
        }

        session.channel_success(channel_id).ok();
        channel.close().await.ok();

        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        tracing::trace!(peer_address = self.peer_addr, "window_change_request");
        if let Some(client_state) = self.get_client_state_or_report_error(session, channel) {
            client_state
                .bmc_connection
                .to_bmc_msg_tx
                .send(ToBmcMessage::ChannelMsg(ChannelMsg::WindowChange {
                    col_width,
                    row_height,
                    pix_width,
                    pix_height,
                }))
                .await
                .map_err(|_| HandlerError::WritingToChannel {
                    what: "window change request",
                })?;
        }
        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum HandlerError {
    #[error("BUG: {method} called but we don't have an authenticated user")]
    MissingAuthenticatedUser { method: &'static str },
    #[error("could not get BMC connection for {machine}: {error}")]
    GettingBmcConnection {
        machine: String,
        error: GetConnectionError,
    },
    #[error("error replying with {what} to {method}: {error}")]
    Replying {
        method: &'static str,
        what: &'static str,
        error: russh::Error,
    },
    #[error("error writing {what} to channel: BMC disconnected?")]
    WritingToChannel { what: &'static str },
    #[error("BMC connection for {machine_id} dropped before we could subscribe to messages")]
    BmcDisconnectedBeforeSubscribe { machine_id: MachineId },
    #[error("error performing pubkey auth via admin authorized_keys for {machine_id}: {error}")]
    PubkeyAuthAdminAuthorizedKeys {
        machine_id: String,
        error: PubkeyAuthError,
    },
    #[error("error validating pubkey with carbide-api for instance {instance_id}: {error}")]
    PubkeyAuthTenant {
        instance_id: String,
        error: PubkeyAuthError,
    },
    #[error(transparent)]
    Russh(#[from] russh::Error),
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum PubkeyAuthError {
    #[error("error reading authorized_keys file at {path}: {error}")]
    ReadingAuthorizedKeys {
        path: String,
        error: russh::keys::ssh_key::Error,
    },
    #[error("unexpected error calling carbide-api to validate pubkey for {user}: {tonic_status}")]
    CarbideApi {
        user: String,
        tonic_status: tonic::Status,
    },
}

/// Indicates the reason auth may have failed. This is so we can avoid logging warnings about failed authentication if only the first method (pubkey) failed but the second succeeded.
enum AuthFailureReason {
    PubKey {
        machine_string: String,
    },
    Certificate {
        machine_string: String,
        user: Option<String>,
    },
}

impl AuthFailureReason {
    fn metric(&self) -> &'static [opentelemetry::KeyValue] {
        match self {
            AuthFailureReason::PubKey { .. } => PUBKEY_AUTH_FAILURE_METRIC.as_slice(),
            AuthFailureReason::Certificate { .. } => CERT_AUTH_FAILURE_METRIC.as_slice(),
        }
    }

    fn machine_string(&self) -> &str {
        match self {
            AuthFailureReason::PubKey { machine_string, .. } => machine_string,
            AuthFailureReason::Certificate { machine_string, .. } => machine_string,
        }
    }

    fn detected_user(&self) -> Option<&str> {
        match self {
            AuthFailureReason::PubKey { .. } => None,
            AuthFailureReason::Certificate { user, .. } => user.as_ref().map(String::as_str),
        }
    }
}

/// Check if the user is in the configured authorized_keys file, which grants them admin access (can
/// log into any host.) This is generally only used for testing: In production we should be using
/// OpenSSH certificate auth, or no admin auth at all.
fn pubkey_auth_admin_authorized_keys(
    public_key: &PublicKey,
    config: &Config,
    user: &str,
) -> Result<bool, PubkeyAuthError> {
    let Some(authorized_keys_path) = config.authorized_keys_path.as_ref() else {
        return Ok(false);
    };

    let authorized_keys = AuthorizedKeys::read_file(authorized_keys_path).map_err(|error| {
        PubkeyAuthError::ReadingAuthorizedKeys {
            path: authorized_keys_path.to_string_lossy().to_string(),
            error,
        }
    })?;

    if authorized_keys.iter().any(|entry| {
        entry
            .public_key()
            .public_key_base64()
            .eq(&public_key.public_key_base64())
    }) {
        tracing::info!(user, "accepting admin public key via authorized_keys");
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Authenticate the given pubkey via carbide-api, assuming the username is an instance ID.
async fn pubkey_auth_tenant(
    user: &str,
    public_key: &PublicKey,
    forge_api_client: &ForgeApiClient,
) -> Result<bool, PubkeyAuthError> {
    let authorized = match forge_api_client
        .validate_tenant_public_key(ValidateTenantPublicKeyRequest {
            instance_id: user.to_string(),
            tenant_public_key: public_key.public_key_base64(),
        })
        .await
    {
        // carbide-api has a weird way of just returning an internal error if the given pubkey is
        // not allowed to authenticate to this machine, rather than returning a valid-but-negative
        // response. So if it didn't fail, it's allowed. If it failed with an internal server error,
        // that's a rejection. If it failed for another reason, bubble up an error here (it will
        // still cause a reject.)
        Ok(_) => {
            tracing::info!(
                user,
                "accepting public key via carbide validate_tenant_public_key"
            );
            true
        }
        Err(tonic_status) => match tonic_status.code() {
            Code::Internal | Code::NotFound => {
                // Internal means the key doesn't match, NotFound means there's no instance like this
                tracing::debug!(
                    user,
                    "rejecting public key via carbide validate_tenant_public_key"
                );
                false
            }
            Code::InvalidArgument => {
                // InvalidArgument can happen if the user is not a valid instance ID.
                tracing::warn!(
                    user,
                    "InvalidArgument when validating public key via carbide-api"
                );
                false
            }
            _ => {
                // Any other error, we should just reject, even if the config overrides it, to stop
                // bugs.
                return Err(PubkeyAuthError::CarbideApi {
                    tonic_status,
                    user: user.to_owned(),
                });
            }
        },
    };

    Ok(authorized)
}
