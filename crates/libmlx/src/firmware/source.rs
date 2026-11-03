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

// src/source.rs
// Defines the FirmwareSource enum for resolving firmware files from
// different locations: local filesystem, HTTPS, and SSH.

use std::path::{Path, PathBuf};
use std::time::Duration;

use forge_ssh::ssh_client::{AuthConfig, HostKeyVerification, SshClientConfig};
use tokio::io::AsyncWriteExt;
use tracing;

use crate::firmware::credentials::Credentials;
use crate::firmware::error::{FirmwareError, FirmwareResult};

// Administrative network limit per artifact, matching the long firmware-command
// budget. It includes connection, authentication, and the complete response.
const SOURCE_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30 * 60);

// FirmwareSource represents a firmware binary location. Supported
// source types are local filesystem, HTTPS, and SSH/SCP.
pub enum FirmwareSource {
    // Local references a file on the local filesystem. Accepts
    // both absolute and relative paths, with or without a file://
    // prefix.
    Local {
        path: PathBuf,
    },
    // Http downloads firmware from an HTTPS (or HTTP) URL with
    // optional authentication credentials.
    Http {
        url: String,
        credentials: Option<Credentials>,
    },
    // Ssh fetches firmware from a remote host via SSH, using
    // key-based or agent authentication. Uses base64 encoding
    // for binary-safe transfer.
    Ssh(SshSource),
}

impl FirmwareSource {
    // local creates a Local source from a filesystem path.
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::Local { path: path.into() }
    }

    // http creates an Http source for the given URL.
    pub fn http(url: impl Into<String>) -> Self {
        Self::Http {
            url: url.into(),
            credentials: None,
        }
    }

    // ssh creates an Ssh source with the given host and remote path.
    // Defaults to port 22 and the current user.
    pub fn ssh(host: impl Into<String>, remote_path: impl Into<String>) -> Self {
        Self::Ssh(SshSource {
            host: host.into(),
            port: 22,
            username: whoami().unwrap_or_else(|| "root".to_string()),
            remote_path: remote_path.into(),
            credentials: None,
        })
    }

    /// `from_url` parses a URL string into a `FirmwareSource`. The URL
    /// prefix determines the source type:
    ///
    /// - `https://` or `http://` -> `Http`
    /// - `ssh://[user@]host:path` -> `Ssh` (SCP-style colon separator)
    /// - `file://path` -> `Local` (strips the prefix)
    /// - anything else -> `Local` (treated as a filesystem path)
    ///
    /// IPv6 SSH hosts use brackets: `ssh://user@[2001:db8::1]:path`.
    /// SSH sources use port 22. An omitted username uses the `USER` environment
    /// variable, falling back to `LOGNAME` and then `root` when unavailable.
    /// Remote paths may be absolute or relative and preserve colons and `@`.
    /// Empty hosts or paths, or a missing host/path separator, return an error.
    pub fn from_url(url: &str) -> FirmwareResult<Self> {
        if url.starts_with("https://") || url.starts_with("http://") {
            Ok(Self::http(url))
        } else if url.starts_with("ssh://") {
            let (host, username, remote_path) = parse_ssh_url(url)?;
            Ok(Self::Ssh(SshSource {
                host,
                port: 22,
                username,
                remote_path,
                credentials: None,
            }))
        } else if let Some(path) = url.strip_prefix("file://") {
            Ok(Self::local(path))
        } else {
            Ok(Self::local(url))
        }
    }

    // with_credentials sets the authentication credentials.
    // For Http sources, the credential must be an HTTP type
    // (BearerToken, BasicAuth, Header). For Ssh sources, it
    // must be an SSH type (SshKey, SshAgent). Validation happens
    // at resolve time.
    pub fn with_credentials(mut self, cred: Credentials) -> Self {
        match &mut self {
            Self::Http { credentials, .. } => *credentials = Some(cred),
            Self::Ssh(SshSource { credentials, .. }) => *credentials = Some(cred),
            Self::Local { .. } => {} // no-op for local sources
        }
        self
    }

    // with_port sets the SSH port. Only affects Ssh sources.
    pub fn with_port(mut self, p: u16) -> Self {
        if let Self::Ssh(SshSource { port, .. }) = &mut self {
            *port = p;
        }
        self
    }

    // with_username sets the SSH username. Only affects Ssh sources.
    pub fn with_username(mut self, user: impl Into<String>) -> Self {
        if let Self::Ssh(SshSource { username, .. }) = &mut self {
            *username = user.into();
        }
        self
    }

    /// `resolve` returns a local file path, downloading remote artifacts into
    /// `work_dir`. Each HTTP or SSH transfer has one 30-minute network deadline,
    /// including connection, authentication, and the complete response, without
    /// retries. Local disk writes finish outside that deadline because Tokio
    /// filesystem work can continue after its future is dropped.
    pub async fn resolve(&self, work_dir: &Path) -> FirmwareResult<PathBuf> {
        match self {
            Self::Local { path } => resolve_local(path).await,
            Self::Http { url, credentials } => {
                resolve_http(
                    url,
                    credentials.as_ref(),
                    work_dir,
                    SOURCE_TRANSFER_TIMEOUT,
                    reqwest::redirect::Policy::default(),
                )
                .await
            }
            Self::Ssh(ssh_source) => {
                resolve_ssh(ssh_source, work_dir, None, SOURCE_TRANSFER_TIMEOUT).await
            }
        }
    }

    /// Checked updates accept only unauthenticated HTTP(S), with no redirects.
    /// Reuse the legacy transfer deadline and file-writing path, but never let
    /// a redirect change the source policy established at admission.
    pub(super) async fn resolve_for_update(&self, work_dir: &Path) -> FirmwareResult<PathBuf> {
        let Self::Http {
            url,
            credentials: None,
        } = self
        else {
            return Err(FirmwareError::ConfigError(
                "firmware updates require unauthenticated HTTP(S) sources".into(),
            ));
        };
        super::config::validate_update_source(url)?;
        resolve_http(
            url,
            None,
            work_dir,
            SOURCE_TRANSFER_TIMEOUT,
            reqwest::redirect::Policy::none(),
        )
        .await
    }

    // description returns a human-readable description of the
    // firmware source location, suitable for logging.
    pub fn description(&self) -> String {
        match self {
            Self::Local { path } => format!("local:{}", path.display()),
            Self::Http { url, .. } => format!("http:{url}"),
            Self::Ssh(SshSource {
                host,
                port,
                username,
                remote_path,
                ..
            }) => format!("ssh://{username}@{host}:{port}:{remote_path}"),
        }
    }
}

// resolve_local validates that a local file exists and returns its path.
async fn resolve_local(path: &Path) -> FirmwareResult<PathBuf> {
    tracing::info!(path = %path.display(), "Resolving local source");
    if !path.exists() {
        return Err(FirmwareError::FileNotFound(path.to_path_buf()));
    }
    Ok(path.to_path_buf())
}

// resolve_http downloads firmware from an HTTP(S) URL with optional
// credentials to the work directory.
async fn resolve_http(
    url: &str,
    credentials: Option<&Credentials>,
    work_dir: &Path,
    transfer_timeout: Duration,
    redirects: reqwest::redirect::Policy,
) -> FirmwareResult<PathBuf> {
    // Extract filename from URL, falling back to a generic name.
    let filename = url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.next_back())
                .map(str::to_string)
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "firmware.bin".to_string());

    let dest_path = work_dir.join(&filename);

    tracing::info!(url = %url, "Downloading via HTTP");
    if let Some(creds) = credentials {
        tracing::debug!(credential_type = %credential_type_name(creds), "Using HTTP credentials");
    }

    // Build the HTTP request with optional credentials. The `reqwest-tracing` middleware injects
    // the current span's W3C trace context into the outgoing request (#2438).
    let client = reqwest::Client::builder()
        .redirect(redirects)
        .build()
        .map_err(|error| FirmwareError::HttpError(error.to_string()))?;
    let client = reqwest_middleware::ClientBuilder::new(client)
        .with(reqwest_tracing::TracingMiddleware::default())
        .build();
    let mut request = client.get(url);

    if let Some(creds) = credentials {
        creds.validate_http()?;
        request = match creds {
            Credentials::BearerToken { token } => request.bearer_auth(token),
            Credentials::BasicAuth { username, password } => {
                request.basic_auth(username, Some(password))
            }
            Credentials::Header { name, value } => request.header(name, value),
            _ => request, // validated above
        };
    }

    let bytes = tokio::time::timeout(transfer_timeout, async {
        let response = request
            .send()
            .await
            .map_err(|e| FirmwareError::HttpError(format!("Failed to download from {url}: {e}")))?;

        if !response.status().is_success() {
            return Err(FirmwareError::HttpError(format!(
                "HTTP {} from {url}",
                response.status(),
            )));
        }

        response
            .bytes()
            .await
            .map_err(|e| FirmwareError::HttpError(format!("Failed to read response body: {e}")))
    })
    .await
    .map_err(|_| FirmwareError::SourceTransferTimeout {
        timeout: transfer_timeout,
    })??;

    let mut file = tokio::fs::File::create(&dest_path)
        .await
        .map_err(FirmwareError::Io)?;

    file.write_all(&bytes).await.map_err(FirmwareError::Io)?;
    file.flush().await.map_err(FirmwareError::Io)?;

    tracing::info!(
        destination_path = %dest_path.display(),
        file_size_bytes = bytes.len(),
        "HTTP download complete"
    );

    Ok(dest_path)
}

pub struct SshSource {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub remote_path: String,
    pub credentials: Option<Credentials>,
}

// resolve_ssh fetches firmware from a remote host via SSH using
// base64 encoding for binary-safe transfer.
async fn resolve_ssh(
    ssh_source: &SshSource,
    work_dir: &Path,
    known_hosts_file: Option<&Path>,
    transfer_timeout: Duration,
) -> FirmwareResult<PathBuf> {
    let SshSource {
        host,
        port,
        username,
        remote_path,
        credentials,
    } = ssh_source;

    let dest_path = work_dir.join(
        Path::new(remote_path)
            .file_name()
            .unwrap_or("firmware.bin".as_ref()),
    );

    tracing::info!(
        host = %host,
        port = %port,
        user = %username,
        path = %remote_path,
        "Downloading via SSH"
    );

    if let Some(creds) = credentials {
        tracing::debug!(credential_type = %credential_type_name(creds), "Using SSH credentials");
    }

    let auth = credentials.clone().map(AuthConfig::try_from).transpose()?;

    let host_key_verification = match known_hosts_file {
        Some(path) => HostKeyVerification::KnownHostsFile(path.to_path_buf()),
        None => HostKeyVerification::DefaultKnownHostsFile,
    };

    // Transfer the file over SSH using base64 encoding. Base64 keeps the
    // command output text-safe before we decode it back to bytes locally.
    // Use `cat | base64` for portability across Linux (coreutils) and
    // macOS (BSD). Positional file args and the -w0 flag are not portable.
    let command = format!("cat {} | base64", shell_escape(remote_path));
    let result = tokio::time::timeout(transfer_timeout, async {
        let client = SshClientConfig {
            host,
            port: *port,
            username,
            auth: auth.as_ref(),
            host_key_verification,
        }
        .make_authenticated_client()
        .await?;
        client.execute_ssh_command(&command).await
    })
    .await
    .map_err(|_| FirmwareError::SourceTransferTimeout {
        timeout: transfer_timeout,
    })??;

    if result.exit_status != 0 {
        return Err(FirmwareError::CommandFailed(format!(
            "Remote command failed (exit {}): {}",
            result.exit_status,
            result.stderr.trim()
        )));
    }

    // Decode the base64 output back to raw bytes. Strip all
    // whitespace first since both Linux and macOS base64 wrap
    // output at 76 characters by default.
    use base64::Engine;
    let mut b64 = result.stdout;
    b64.retain(|c| !c.is_whitespace());
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .map_err(|e| {
            FirmwareError::CommandFailed(format!("Failed to decode base64 transfer: {e}"))
        })?;

    tokio::fs::write(&dest_path, &decoded)
        .await
        .map_err(FirmwareError::Io)?;

    tracing::info!(
        destination_path = %dest_path.display(),
        file_size_bytes = decoded.len(),
        "SSH download complete"
    );

    Ok(dest_path)
}

// credential_type_name returns a human-readable name for a credential
// type, safe for logging (never includes the actual secret).
fn credential_type_name(cred: &Credentials) -> &'static str {
    match cred {
        Credentials::BearerToken { .. } => "bearer_token",
        Credentials::BasicAuth { .. } => "basic_auth",
        Credentials::Header { .. } => "header",
        Credentials::SshKey { .. } => "ssh_key",
        Credentials::SshAgent {} => "ssh_agent",
    }
}

// parse_ssh_url parses an SCP-style SSH URL into its components.
// Format: ssh://[user@]host:path
//
// The colon separates host from path (SCP convention). This supports
// both relative and absolute remote paths:
//   ssh://user@host:relative/path   -> relative path from home dir
//   ssh://user@host:/absolute/path  -> absolute path
//
// IPv6 hosts use brackets, as in `ssh://user@[2001:db8::1]:path`.
// Return the host without brackets for the SSH client's socket lookup.
//
// User defaults to the current user if omitted.
fn parse_ssh_url(url: &str) -> FirmwareResult<(String, String, String)> {
    let stripped = url
        .strip_prefix("ssh://")
        .ok_or_else(|| FirmwareError::ConfigError(format!("Not an SSH URL: '{url}'")))?;

    // An `@` after the first colon belongs to the path, not the username.
    let (username, host_and_path) = match stripped.split_once('@') {
        Some((username, host_and_path)) if !username.contains(':') => {
            (username.to_string(), host_and_path)
        }
        _ => (whoami().unwrap_or_else(|| "root".to_string()), stripped),
    };

    let (host, remote_path) = if let Some(bracketed) = host_and_path.strip_prefix('[') {
        bracketed.split_once("]:")
    } else {
        host_and_path.split_once(':')
    }
    .ok_or_else(|| {
        FirmwareError::ConfigError(format!(
            "SSH URL must use ssh://[user@]host:path format (bracket IPv6 hosts), got: '{url}'"
        ))
    })?;

    if remote_path.is_empty() {
        return Err(FirmwareError::ConfigError(format!(
            "SSH URL missing remote file path: '{url}'"
        )));
    }

    if host.is_empty() {
        return Err(FirmwareError::ConfigError(format!(
            "SSH URL missing host: '{url}'"
        )));
    }

    Ok((host.to_string(), username, remote_path.to_string()))
}

// whoami returns the current username, if available.
fn whoami() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
}

// shell_escape performs basic shell escaping for a path string to
// prevent command injection in SSH commands.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use base64::Engine;
    use forge_ssh::ssh_client::tests::TestSshServer;
    use russh::keys::known_hosts::learn_known_hosts_path;
    use russh::keys::signature::digest::common::getrandom::SysRng;
    use russh::keys::ssh_key::LineEnding;
    use russh::keys::ssh_key::rand_core::UnwrapErr;
    use tempfile::tempdir;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task::JoinSet;

    use super::*;

    /// Checked downloads reject the redirect itself, while the public legacy
    /// resolver must still follow it and preserve the destination's bytes.
    #[tokio::test]
    async fn download_redirect_policy_preserves_legacy_and_rejects_checked_updates() {
        #[derive(Clone, Copy, Debug)]
        enum Submission {
            Legacy,
            Checked,
        }

        for submission in [Submission::Legacy, Submission::Checked] {
            let directory = tempdir().unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/image.bin", listener.local_addr().unwrap());
            let (stop, stopped) = oneshot::channel();
            let mut server = JoinSet::new();
            server.spawn(async move {
                tokio::pin!(stopped);
                let mut requests = 0;
                loop {
                    let (stream, _) = tokio::select! {
                        _ = &mut stopped => return requests,
                        accepted = listener.accept() => accepted.unwrap(),
                    };
                    let mut stream = BufReader::new(stream);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                        if line == "\r\n" {
                            break;
                        }
                    }
                    requests += 1;
                    let response = if requests == 1 {
                        b"HTTP/1.1 302 Found\r\nLocation: /redirected.bin\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".as_slice()
                    } else {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nimage".as_slice()
                    };
                    stream.get_mut().write_all(response).await.unwrap();
                }
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                let source = FirmwareSource::http(url);
                let result = match submission {
                    Submission::Legacy => source.resolve(directory.path()).await,
                    Submission::Checked => source.resolve_for_update(directory.path()).await,
                };
                stop.send(()).unwrap();
                let requests = server.join_next().await.unwrap().unwrap();
                match submission {
                    Submission::Legacy => {
                        let path = result.expect("legacy download follows redirects");
                        assert_eq!(std::fs::read(path).unwrap(), b"image");
                        assert_eq!(requests, 2);
                    }
                    Submission::Checked => {
                        let error = result.expect_err("checked download rejects redirects");
                        let FirmwareError::HttpError(message) = error else {
                            panic!("expected HTTP redirect rejection, got {error:?}");
                        };
                        assert!(message.starts_with("HTTP 302"), "{message}");
                        assert_eq!(requests, 1);
                        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{submission:?} redirect handling exceeds watchdog"));
        }
    }

    /// A successful response header must not leave an unfinished body unbounded.
    #[tokio::test]
    async fn http_transfer_deadline_includes_the_response_body() {
        let directory = tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/image.bin", listener.local_addr().unwrap());
        let (ready, body_started) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let mut server = JoinSet::new();
        server.spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            loop {
                line.clear();
                assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            stream
                .get_mut()
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\npartial image")
                .await
                .unwrap();
            stream.get_mut().flush().await.unwrap();
            ready.send(()).unwrap();
            released.await.unwrap();
        });

        let transfer_timeout = Duration::from_secs(1);
        tokio::time::timeout(Duration::from_secs(5), async {
            let transfer = resolve_http(&url, None, directory.path(), transfer_timeout, reqwest::redirect::Policy::default());
            tokio::pin!(transfer);
            tokio::select! {
                started = body_started => started.unwrap(),
                result = &mut transfer => panic!("transfer finished before the body was held: {result:?}"),
            }
            assert!(matches!(
                transfer.await,
                Err(FirmwareError::SourceTransferTimeout { timeout })
                    if timeout == transfer_timeout
            ));
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
            release.send(()).unwrap();
            server.join_next().await.unwrap().unwrap();
        })
        .await
        .expect("the transfer deadline settles an incomplete HTTP body");
    }

    #[tokio::test]
    async fn resolve_ssh_fetches_firmware_from_russh_server()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempdir()?;
        let work_dir = temp_dir.path().join("work");
        tokio::fs::create_dir(&work_dir).await?;

        let mut rng = SysRng;
        let client_key = russh::keys::PrivateKey::random(
            &mut UnwrapErr(&mut rng),
            russh::keys::Algorithm::Ed25519,
        )?;
        let client_key_path = temp_dir.path().join("id_ed25519");
        client_key.write_openssh_file(&client_key_path, LineEnding::LF)?;

        let remote_path = "/firmware/mlx-fw.bin".to_string();
        let firmware = b"firmware bytes\0with binary data\n".to_vec();
        let firmware_encoded = base64::engine::general_purpose::STANDARD.encode(&firmware);
        let server = TestSshServer::spawn(
            "firmware-user".to_string(),
            client_key.public_key().clone(),
            HashMap::from([(
                format!("cat {} | base64", shell_escape(&remote_path)),
                firmware_encoded,
            )]),
        )
        .await?;

        let known_hosts_path = temp_dir.path().join(".ssh").join("known_hosts");
        learn_known_hosts_path(
            "127.0.0.1",
            server.port,
            &server.host_public_key,
            &known_hosts_path,
        )?;

        let ssh_source = SshSource {
            host: "127.0.0.1".into(),
            port: server.port,
            username: "firmware-user".into(),
            remote_path,
            credentials: Some(Credentials::ssh_key(
                client_key_path.to_string_lossy().into_owned(),
            )),
        };

        let resolved_path = resolve_ssh(
            &ssh_source,
            &work_dir,
            Some(known_hosts_path.as_path()),
            SOURCE_TRANSFER_TIMEOUT,
        )
        .await?;

        assert_eq!(resolved_path, work_dir.join("mlx-fw.bin"));
        assert_eq!(tokio::fs::read(resolved_path).await?, firmware);

        Ok(())
    }
}

#[cfg(test)]
mod coverage_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, Check, check_cases_async, scenarios, value_scenarios};

    use super::*;

    // from_url routes a URL string to the right source variant. We project the
    // parsed source to description() (a total, deterministic projection) so each
    // row pins both the chosen variant and the carried fields. FirmwareError is
    // not PartialEq, so rejection rows use Fails + map_err(drop). Every ssh row
    // that yields uses an explicit user so the description is whoami-independent.
    #[test]
    fn from_url_routes_to_the_right_variant() {
        scenarios!(
            run = |url| {
                FirmwareSource::from_url(url)
                    .map(|s| s.description())
                    .map_err(drop)
            };
            "https URL becomes an Http source" {
                "https://example.com/fw.bin" => Yields("http:https://example.com/fw.bin".to_string()),
            }

            "http URL becomes an Http source" {
                "http://example.com/fw.bin" => Yields("http:http://example.com/fw.bin".to_string()),
            }

            "file:// prefix is stripped to a Local path" {
                "file:///abs/path/fw.bin" => Yields("local:/abs/path/fw.bin".to_string()),
            }

            "file:// with a relative remainder" {
                "file://relative/fw.bin" => Yields("local:relative/fw.bin".to_string()),
            }

            "bare absolute path is a Local source" {
                "/plain/path/fw.bin" => Yields("local:/plain/path/fw.bin".to_string()),
            }

            "bare relative path is a Local source" {
                "relative/fw.bin" => Yields("local:relative/fw.bin".to_string()),
            }

            "ssh with explicit user and absolute path" {
                "ssh://deploy@host.example:/abs/fw.bin" => Yields("ssh://deploy@host.example:22:/abs/fw.bin".to_string()),
            }

            "ssh with explicit user and relative path" {
                "ssh://deploy@host.example:rel/fw.bin" => Yields("ssh://deploy@host.example:22:rel/fw.bin".to_string()),
            }

            "ssh URL with no colon separator is rejected" {
                "ssh://hostonly" => Fails,
            }

            "ssh URL with an empty remote path is rejected" {
                "ssh://deploy@host.example:" => Fails,
            }

            "ssh URL with an empty host is rejected" {
                "ssh://:/abs/fw.bin" => Fails,
            }
        );
    }

    // An ssh:// URL with no explicit user still parses (username defaults to the
    // current user); we only assert that it succeeds since whoami is environment
    // dependent.
    #[test]
    fn from_url_defaults_ssh_user() {
        Case {
            scenario: "ssh without an explicit user parses",
            input: "ssh://host.example:/abs/fw.bin",
            expect: Yields(()),
        }
        .check(|url| FirmwareSource::from_url(url).map(|_| ()).map_err(drop));
    }

    #[tokio::test]
    async fn local_source_resolution_cases() {
        let temp_dir = tempfile::tempdir().unwrap();
        let work_dir = temp_dir.path().to_path_buf();
        let present_path = work_dir.join("firmware.bin");
        let missing_path = work_dir.join("missing.bin");
        tokio::fs::write(&present_path, b"firmware").await.unwrap();

        check_cases_async(
            [
                Case {
                    scenario: "an existing local source resolves to itself",
                    input: FirmwareSource::local(present_path.clone()),
                    expect: Yields(present_path),
                },
                Case {
                    scenario: "a missing local source reports its exact path",
                    input: FirmwareSource::local(missing_path.clone()),
                    expect: FailsWith(missing_path),
                },
            ],
            |source| {
                let work_dir = work_dir.clone();
                async move {
                    source
                        .resolve(&work_dir)
                        .await
                        .map_err(|error| match error {
                            FirmwareError::FileNotFound(path) => path,
                            error => panic!("unexpected local source error: {error:?}"),
                        })
                }
            },
        )
        .await;
    }

    // Check the host, username, and remote path. The default username comes
    // from the test's environment. Compare error text because FirmwareError
    // is not PartialEq.
    #[test]
    fn parse_ssh_url_splits_components() {
        let default_user = whoami().unwrap_or_else(|| "root".to_string());

        scenarios!(
            run = |url| parse_ssh_url(url).map_err(|error| error.to_string());
            "user, host, absolute path" {
                "ssh://deploy@host.example:/abs/fw.bin" => Yields(("host.example".to_string(), "deploy".to_string(), "/abs/fw.bin".to_string())),
            }

            "user, host, relative path" {
                "ssh://deploy@host.example:rel/fw.bin" => Yields(("host.example".to_string(), "deploy".to_string(), "rel/fw.bin".to_string())),
            }

            "no user, host, absolute path" {
                "ssh://host.example:/abs/fw.bin" => Yields(("host.example".to_string(), default_user.clone(), "/abs/fw.bin".to_string())),
            }

            "user, bracketed IPv6 host, relative path" {
                "ssh://deploy@[2001:db8::1]:rel/fw.bin" => Yields(("2001:db8::1".to_string(), "deploy".to_string(), "rel/fw.bin".to_string())),
            }

            "no user, bracketed IPv6 host, absolute path" {
                "ssh://[2001:db8::1]:/abs/fw.bin" => Yields(("2001:db8::1".to_string(), default_user.clone(), "/abs/fw.bin".to_string())),
            }

            "path containing a colon keeps the rest after the first colon" {
                "ssh://host.example:/abs:with:colons" => Yields(("host.example".to_string(), default_user.clone(), "/abs:with:colons".to_string())),
            }

            "an at sign in the path is not a username separator" {
                "ssh://host.example:/fw@release.bin" => Yields(("host.example".to_string(), default_user.clone(), "/fw@release.bin".to_string())),
            }

            "not an ssh URL (missing prefix)" {
                "https://host.example/fw.bin" => Fails,
            }

            "missing the colon separator" {
                "ssh://hostonly" => Fails,
            }

            "IPv6 host missing the colon after the closing bracket" {
                "ssh://[2001:db8::1]/abs/fw.bin" => FailsWith("configuration error: SSH URL must use ssh://[user@]host:path format (bracket IPv6 hosts), got: 'ssh://[2001:db8::1]/abs/fw.bin'".to_string()),
            }

            "empty remote path" {
                "ssh://deploy@host.example:" => Fails,
            }

            "empty bracketed host" {
                "ssh://[]:/abs/fw.bin" => Fails,
            }

            "empty host with a user" {
                "ssh://deploy@:/abs/fw.bin" => Fails,
            }

            "empty host without a user" {
                "ssh://:/abs/fw.bin" => Fails,
            }
        );
    }

    // description() formats each variant. Builders set deterministic fields so the
    // ssh rows are whoami-independent. Local/Http carry their raw field verbatim.
    #[test]
    fn description_formats_each_variant() {
        value_scenarios!(
            run = |src| src.description();
            "local path" {
                FirmwareSource::local("/tmp/fw.bin") => "local:/tmp/fw.bin".to_string(),
            }

            "http url" {
                FirmwareSource::http("https://example.com/fw.bin") => "http:https://example.com/fw.bin".to_string(),
            }

            "ssh with explicit user, port, host, path" {
                FirmwareSource::ssh("host.example", "/abs/fw.bin")
                .with_username("deploy")
                .with_port(2222) => "ssh://deploy@host.example:2222:/abs/fw.bin".to_string(),
            }
        );
    }

    // The ssh() constructor defaults to port 22; with_port overrides it and only
    // affects Ssh sources. We read the port back through description().
    #[test]
    fn ssh_constructor_and_builders_set_fields() {
        // Default port is 22.
        Check {
            scenario: "ssh defaults to port 22",
            input: FirmwareSource::ssh("h", "/p").with_username("u"),
            expect: "ssh://u@h:22:/p".to_string(),
        }
        .check(|src| src.description());

        // with_port and with_username are no-ops on non-ssh sources, so the
        // description is unchanged.
        value_scenarios!(
            run = |src| src.description();
            "with_port is a no-op on Local" {
                FirmwareSource::local("/tmp/fw.bin").with_port(9999) => "local:/tmp/fw.bin".to_string(),
            }

            "with_username is a no-op on Http" {
                FirmwareSource::http("https://x/fw").with_username("ignored") => "http:https://x/fw".to_string(),
            }
        );
    }

    // with_credentials attaches a credential to Http and Ssh sources and is a
    // no-op on Local. We project to whether credentials ended up Some so the row
    // pins the variant-specific behavior without exposing secrets.
    #[test]
    fn with_credentials_targets_http_and_ssh_only() {
        value_scenarios!(
            run = |src| match src {
                FirmwareSource::Http { credentials, .. } => credentials.is_some(),
                FirmwareSource::Ssh(SshSource { credentials, .. }) => credentials.is_some(),
                FirmwareSource::Local { .. } => false,
            };
            "Http source stores the credential" {
                FirmwareSource::http("https://x/fw")
                .with_credentials(Credentials::bearer_token("t")) => true,
            }

            "Ssh source stores the credential" {
                FirmwareSource::ssh("h", "/p")
                .with_credentials(Credentials::ssh_agent()) => true,
            }

            "Local source ignores the credential" {
                FirmwareSource::local("/tmp/fw.bin")
                .with_credentials(Credentials::bearer_token("t")) => false,
            }
        );
    }

    // credential_type_name maps each Credentials variant to a stable, secret-free
    // label. One row per variant.
    #[test]
    fn credential_type_name_covers_every_variant() {
        value_scenarios!(
            run = |cred| credential_type_name(&cred);
            "bearer token" {
                Credentials::bearer_token("t") => "bearer_token",
            }

            "basic auth" {
                Credentials::basic_auth("u", "p") => "basic_auth",
            }

            "header" {
                Credentials::header("X-Key", "v") => "header",
            }

            "ssh key" {
                Credentials::ssh_key("/k") => "ssh_key",
            }

            "ssh agent" {
                Credentials::ssh_agent() => "ssh_agent",
            }
        );
    }

    // shell_escape single-quotes its argument and escapes embedded single quotes
    // as '\'' so the result is safe to splice into an SSH command. Covers the
    // no-special-chars, single-quote, leading-quote, and empty cases.
    #[test]
    fn shell_escape_quotes_and_escapes() {
        value_scenarios!(
            run = shell_escape;
            "plain path is wrapped in single quotes" {
                "/firmware/fw.bin" => "'/firmware/fw.bin'".to_string(),
            }

            "empty string" {
                "" => "''".to_string(),
            }

            "single embedded quote" {
                "a'b" => "'a'\\''b'".to_string(),
            }

            "leading quote" {
                "'x" => "''\\''x'".to_string(),
            }

            "two embedded quotes" {
                "a'b'c" => "'a'\\''b'\\''c'".to_string(),
            }
        );
    }
}
