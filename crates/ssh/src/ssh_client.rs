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

use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;
use std::sync::Arc;
use std::time::Duration;

use russh::ChannelMsg;
use russh::client::{AuthResult, Handle};
use russh::keys::agent::AgentIdentity;
use russh::keys::{PrivateKeyWithHashAlg, PublicKey};

use crate::{SshError, SshResult};

/// Simple SSH client that can execute commands and return results, wrapping russh::client::Handle
pub struct SshClient {
    inner: Handle<KnownHostsCheck>,
}

pub enum AuthConfig {
    // SshKey uses a private key file for SSH authentication, with
    // an optional passphrase.
    SshKey {
        path: String,
        passphrase: Option<String>,
    },
    // SshAgent uses the running SSH agent for authentication. The
    // agent is reached via the SSH_AUTH_SOCK environment variable.
    SshAgent,
    Password {
        password: String,
    },
}

pub struct SshClientConfig<'a> {
    pub host: &'a str,
    pub port: u16,
    pub username: &'a str,
    pub auth: Option<&'a AuthConfig>,
    pub host_key_verification: HostKeyVerification,
}

/// How to verify the SSH server's host key.
pub enum HostKeyVerification {
    /// Skip host key verification.
    Insecure,
    /// Verify against the system default known_hosts file (`~/.ssh/known_hosts`).
    DefaultKnownHostsFile,
    /// Verify against an explicit known_hosts file.
    KnownHostsFile(PathBuf),
}

impl<'a> SshClientConfig<'a> {
    pub async fn make_authenticated_client(self) -> SshResult<SshClient> {
        let mut client = russh::client::connect(
            russh_client_config(),
            (self.host, self.port),
            KnownHostsCheck {
                host: self.host.to_string(),
                port: self.port,
                check: self.host_key_verification,
            },
        )
        .await?;

        match self.auth {
            Some(AuthConfig::SshKey {
                path, passphrase, ..
            }) => {
                authenticate_with_key(
                    &mut client,
                    self.username,
                    Path::new(path),
                    passphrase.as_deref(),
                )
                .await?
            }
            Some(AuthConfig::SshAgent) => {
                authenticate_with_agent(&mut client, self.username).await?
            }
            Some(AuthConfig::Password { password }) => {
                match client
                    .authenticate_password(self.username, password)
                    .await?
                {
                    AuthResult::Success => {}
                    AuthResult::Failure { .. } => {
                        return Err(SshError::AuthenticationFailed(format!(
                            "password authentication failed for {} at {}",
                            self.username, self.host,
                        )));
                    }
                }
            }
            None => {
                // Default to key file at ~/.ssh/id_rsa.
                let key_path = default_ssh_key_path();
                authenticate_with_key(&mut client, self.username, &key_path, None).await?;
            }
        }

        Ok(SshClient { inner: client })
    }
}

impl SshClient {
    pub async fn execute_ssh_command(&self, command: &str) -> SshResult<SshCommandOutput> {
        let mut channel = self.inner.channel_open_session().await?;
        channel.exec(true, command).await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_status = None;

        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext: 1 } => stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus {
                    exit_status: status,
                } => exit_status = Some(status),
                _ => {}
            }
        }

        let Some(exit_status) = exit_status else {
            return Err(SshError::CommandDidNotExit);
        };

        // Convert stdout/stderr to strings, only allocating if there is a failure. (FromUtf8Error lets
        // you get the original bytes in as_bytes, and we can build a new string via from_utf8_lossy
        // only if it fails.)
        let stdout = String::from_utf8(stdout)
            .unwrap_or_else(|e: FromUtf8Error| String::from_utf8_lossy(e.as_bytes()).to_string());
        let stderr = String::from_utf8(stderr)
            .unwrap_or_else(|e: FromUtf8Error| String::from_utf8_lossy(e.as_bytes()).to_string());

        Ok(SshCommandOutput {
            stdout,
            stderr,
            exit_status,
        })
    }
}

/// Configuration for russh's SSH client connections
fn russh_client_config() -> Arc<russh::client::Config> {
    Arc::new(russh::client::Config {
        // Some BMC's use a Diffie-Hellman group size of 2048, which is not allowed by default.
        gex: russh::client::GexParams::new(2048, 8192, 8192)
            .expect("BUG: static DH group parameters must be valid"),
        keepalive_interval: Some(Duration::from_secs(60)),
        keepalive_max: 2,
        window_size: 2097152 * 3,
        maximum_packet_size: 65535,
        ..Default::default()
    })
}

struct KnownHostsCheck {
    host: String,
    port: u16,
    check: HostKeyVerification,
}

impl russh::client::Handler for KnownHostsCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        match &self.check {
            HostKeyVerification::Insecure => Ok(true),
            HostKeyVerification::KnownHostsFile(path) => {
                russh::keys::check_known_hosts_path(&self.host, self.port, server_public_key, path)
                    .map_err(russh::Error::from)
            }
            HostKeyVerification::DefaultKnownHostsFile => {
                russh::keys::check_known_hosts(&self.host, self.port, server_public_key)
                    .map_err(russh::Error::from)
            }
        }
    }
}

// default_ssh_key_path returns the default SSH private key path (~/.ssh/id_rsa).
fn default_ssh_key_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".ssh").join("id_rsa")
}

pub struct SshCommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: u32,
}

async fn authenticate_with_key(
    client: &mut Handle<KnownHostsCheck>,
    username: &str,
    path: &Path,
    passphrase: Option<&str>,
) -> SshResult<()> {
    let path_display = path.display();
    let private_key = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| SshError::SshKey(format!("Failed to read SSH key '{path_display}': {e}")))?;
    let private_key = russh::keys::decode_secret_key(&private_key, passphrase).map_err(|e| {
        SshError::SshKey(format!(
            "Unable to load SSH key '{path_display}', bad format or passphrase: {e}"
        ))
    })?;
    let hash_alg = client.best_supported_rsa_hash().await?.flatten();

    match client
        .authenticate_publickey(
            username,
            PrivateKeyWithHashAlg::new(Arc::new(private_key), hash_alg),
        )
        .await
        .map_err(|e| SshError::SshKey(format!("Failed to authenticate with key: {e}")))?
    {
        AuthResult::Success => Ok(()),
        AuthResult::Failure { .. } => Err(SshError::SshKey(
            "SSH key authentication failed".to_string(),
        )),
    }
}

async fn authenticate_with_agent(
    client: &mut Handle<KnownHostsCheck>,
    username: &str,
) -> SshResult<()> {
    let mut agent = russh::keys::agent::client::AgentClient::connect_env()
        .await
        .map_err(|e| SshError::SshKey(format!("Failed to connect to SSH agent: {e}")))?;
    let identities = agent.request_identities().await.map_err(|e| {
        SshError::SshKey(format!("Failed to request identities from SSH agent: {e}"))
    })?;

    if identities.is_empty() {
        return Err(SshError::SshKey("SSH agent has no identities".to_string()));
    }

    let hash_alg = client.best_supported_rsa_hash().await?.flatten();

    for identity in identities {
        let auth_result = match identity {
            AgentIdentity::PublicKey { key, .. } => {
                client
                    .authenticate_publickey_with(username, key, hash_alg, &mut agent)
                    .await
            }
            AgentIdentity::Certificate { certificate, .. } => {
                client
                    .authenticate_certificate_with(username, certificate, hash_alg, &mut agent)
                    .await
            }
        };

        if matches!(auth_result, Ok(AuthResult::Success)) {
            return Ok(());
        }
    }

    Err(SshError::SshKey(
        "SSH agent authentication failed".to_string(),
    ))
}

#[cfg(feature = "test_support")]
pub mod tests {
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;

    use russh::keys::signature::digest::common::getrandom::SysRng;
    use russh::keys::ssh_key::rand_core::UnwrapErr;
    use russh::server::{Auth, ChannelOpenHandle, Config, Msg, Server as _, Session, run_stream};
    use russh::{Channel, ChannelId, MethodKind, MethodSet, server};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    use super::*;

    pub struct TestSshServer {
        authorized_user: String,
        authorized_key: PublicKey,
        expected_commands: Arc<HashMap<String, String>>,
    }

    impl TestSshServer {
        pub async fn spawn(
            authorized_user: String,
            authorized_key: PublicKey,
            expected_commands: HashMap<String, String>,
        ) -> Result<TestSshServerHandle, Box<dyn std::error::Error>> {
            let mut rng = SysRng;
            let host_key = russh::keys::PrivateKey::random(
                &mut UnwrapErr(&mut rng),
                russh::keys::Algorithm::Ed25519,
            )?;
            let host_public_key = host_key.public_key().clone();
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let port = listener.local_addr()?.port();
            let config = Arc::new(Config {
                keys: vec![host_key],
                methods: MethodSet::from([MethodKind::PublicKey].as_slice()),
                ..Default::default()
            });
            let mut server = Self {
                authorized_user,
                authorized_key,
                expected_commands: Arc::new(expected_commands),
            };
            let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
            let join_handle = tokio::spawn(async move {
                tokio::select! {
                    accept_result = listener.accept() => {
                        let Ok((socket, peer_addr)) = accept_result else {
                            return;
                        };
                        let handler = server.new_client(Some(peer_addr));
                        let Ok(session) = run_stream(config, socket, handler).await else {
                            return;
                        };
                        let _ = session.await;
                    }
                    _ = &mut shutdown_rx => {}
                }
            });

            Ok(TestSshServerHandle {
                port,
                host_public_key,
                shutdown_tx: Some(shutdown_tx),
                join_handle,
            })
        }
    }

    impl server::Server for TestSshServer {
        type Handler = TestSshHandler;

        fn new_client(&mut self, _addr: Option<SocketAddr>) -> Self::Handler {
            TestSshHandler {
                authorized_user: self.authorized_user.clone(),
                authorized_key: self.authorized_key.clone(),
                expected_commands: self.expected_commands.clone(),
            }
        }
    }

    pub struct TestSshServerHandle {
        pub port: u16,
        pub host_public_key: PublicKey,
        shutdown_tx: Option<oneshot::Sender<()>>,
        join_handle: tokio::task::JoinHandle<()>,
    }

    impl Drop for TestSshServerHandle {
        fn drop(&mut self) {
            if let Some(shutdown_tx) = self.shutdown_tx.take() {
                let _ = shutdown_tx.send(());
            }
            self.join_handle.abort();
        }
    }

    pub struct TestSshHandler {
        authorized_user: String,
        authorized_key: PublicKey,
        expected_commands: Arc<HashMap<String, String>>,
    }

    impl server::Handler for TestSshHandler {
        type Error = russh::Error;

        async fn auth_publickey_offered(
            &mut self,
            user: &str,
            public_key: &PublicKey,
        ) -> Result<Auth, Self::Error> {
            Ok(self.auth_result(user, public_key))
        }

        async fn auth_publickey(
            &mut self,
            user: &str,
            public_key: &PublicKey,
        ) -> Result<Auth, Self::Error> {
            Ok(self.auth_result(user, public_key))
        }

        async fn channel_open_session(
            &mut self,
            _channel: Channel<Msg>,
            reply: ChannelOpenHandle,
            _session: &mut Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        async fn exec_request(
            &mut self,
            channel: ChannelId,
            data: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            let command = std::str::from_utf8(data).map_err(|_| russh::Error::Disconnect)?;
            session.channel_success(channel)?;

            if let Some(response) = self.expected_commands.get(command) {
                // let encoded = base64::engine::general_purpose::STANDARD.encode(&*self.firmware);
                session.data(channel, response.as_bytes().to_vec())?;
                session.exit_status_request(channel, 0)?;
            } else {
                session.extended_data(
                    channel,
                    1,
                    format!("unexpected command: {command}").into_bytes(),
                )?;
                session.exit_status_request(channel, 1)?;
            }

            session.eof(channel)?;
            session.close(channel)?;
            Ok(())
        }
    }

    impl TestSshHandler {
        fn auth_result(&self, user: &str, public_key: &PublicKey) -> Auth {
            if user == self.authorized_user && public_key == &self.authorized_key {
                Auth::Accept
            } else {
                Auth::reject()
            }
        }
    }
}
