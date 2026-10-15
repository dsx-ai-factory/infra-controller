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
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use eyre::Context;
use tokio::io::AsyncBufReadExt;
use tokio::process;
use tokio::sync::oneshot;

use crate::utils::LOCALHOST_CERTS;

const ROOT_TOKEN: &str = "Root Token";
const VAULT_CACERT_ENV_STRING: &str = "$ export VAULT_CACERT";

// A port can be claimed by another process between allocate_port releasing it
// and vault binding it, so a failed start just means "try another port". Give
// it a handful of fresh ports before treating the failure as real.
const MAX_START_ATTEMPTS: usize = 5;

// Bounds a single attempt. A vault that loses the port race exits well before
// this (surfacing as an error right away); the timeout only stops a genuine
// hang from stalling the retry loop.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub struct Vault {
    /// The address vault bound. [`start_on`] chooses it -- retrying across ports
    /// until one sticks -- so callers read it back here instead of picking it.
    pub addr: SocketAddr,
    pub process: process::Child,
    pub token: String,
    /// CA certificate path for `addr`: Vault's dev CA for IPv4, repository CA for IPv6.
    pub ca_cert: String,
    _tls_dir: tempfile::TempDir,
}

/// Start a vault dev server on a free IPv4 loopback port and wait until it is ready.
pub async fn start() -> Result<Vault, eyre::Report> {
    start_on(Ipv4Addr::LOCALHOST.into()).await
}

/// `start_on` starts a Vault dev server on a free port of `bind_ip` and waits
/// for its token and CA certificate. The address must be locally bindable.
/// Use `127.0.0.1` or `::1` for verified TLS.
///
/// vault binds the listen port itself but cannot be asked to pick a free one
/// and report it back, so we allocate a port and hand it over. The port can be
/// claimed in the gap between allocate_port releasing it and vault binding it,
/// so a failed attempt retries on a fresh port. The bound address is returned
/// on the [`Vault`].
///
/// # Errors
///
/// Returns an error if a local listen address cannot be obtained, or startup retries are
/// exhausted because prerequisites are missing, TLS setup or process spawning
/// fails, or Vault does not report readiness or provide its CA certificate.
pub async fn start_on(bind_ip: IpAddr) -> Result<Vault, eyre::Report> {
    let mut last_err = None;
    for attempt in 1..=MAX_START_ATTEMPTS {
        let addr = allocate_port(bind_ip)?;
        match try_start(addr).await {
            Ok(vault) => return Ok(vault),
            Err(e) => {
                // No logger here. A lost port race is expected occasionally, so
                // keep per-attempt noise low and let the final error speak.
                eprintln!(
                    "vault failed to start on {addr} (attempt {attempt}/{MAX_START_ATTEMPTS}): {e:#}"
                );
                last_err = Some(e);
            }
        }
    }
    let err = last_err.unwrap_or_else(|| eyre::eyre!("vault never started"));
    Err(err.wrap_err(format!(
        "vault did not start after {MAX_START_ATTEMPTS} attempts"
    )))
}

/// Pick a free local port by binding to port 0 and releasing it immediately.
/// The port is free when this returns, so the caller must claim it promptly;
/// [`start_on`] retries if it loses the race.
fn allocate_port(bind_ip: IpAddr) -> Result<SocketAddr, eyre::Report> {
    let listener = TcpListener::bind((bind_ip, 0)).context("binding free vault port")?;
    listener
        .local_addr()
        .context("reading vault listener address")
}

/// Spawn vault on `addr` and wait for it to report a token and CA cert. Errors
/// if vault exits (e.g. the port was taken) or does not report readiness within
/// [`STARTUP_TIMEOUT`], so [`start_on`] can retry on a fresh port.
async fn try_start(addr: SocketAddr) -> Result<Vault, eyre::Report> {
    let bins = crate::utils::find_prerequisites()?;

    // Use an explicit directory under the repository instead of Vault's default
    // temporary directory. Strictly confined packages such as Snap have a private
    // /tmp mount, so the path Vault reports would otherwise be invisible to this
    // process. A unique directory also allows tests to start Vault concurrently.
    let tls_root = crate::utils::REPO_ROOT.join("target/vault-tls");
    std::fs::create_dir_all(&tls_root).context("creating vault TLS root directory")?;
    let tls_dir = tempfile::Builder::new()
        .prefix("vault-")
        .tempdir_in(tls_root)
        .context("creating vault TLS directory")?;

    let mut command =
        tokio::process::Command::new(bins.get("vault").expect("vault command not found in PATH"));
    command
        .arg("server")
        .arg("-dev-tls")
        .arg(format!("-dev-tls-cert-dir={}", tls_dir.path().display()))
        .arg("-dev-no-store-token");
    if addr.is_ipv6() {
        // CI pins Vault 1.14.1, whose dev certificate does not cover `::1`.
        // Keep its dev listener on an unused IPv4 port and add our IPv6
        // listener with the repository certificate, which covers `::1`.
        let config_path = tls_dir.path().join("listener.json");
        let config = serde_json::json!({
            "api_addr": format!("https://{addr}"),
            "listener": {
                "tcp": {
                    "address": addr.to_string(),
                    "tls_cert_file": LOCALHOST_CERTS.server_cert,
                    "tls_key_file": LOCALHOST_CERTS.server_key,
                },
            },
        });
        std::fs::write(&config_path, serde_json::to_vec(&config)?)
            .context("writing IPv6 vault listener configuration")?;
        command
            .arg("-dev-listen-address=127.0.0.1:0")
            .arg("-config")
            .arg(config_path);
    } else {
        command.arg(format!("-dev-listen-address={addr}"));
    }
    let mut process = command
        .env_remove("VAULT_ADDR")
        .env_remove("VAULT_CLIENT_KEY")
        .env_remove("VAULT_CLIENT_CERT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let stdout = tokio::io::BufReader::new(process.stdout.take().unwrap());
    let stderr = tokio::io::BufReader::new(process.stderr.take().unwrap());

    let (token_tx, token_rx) = oneshot::channel();
    let (ca_tx, ca_rx) = oneshot::channel();

    tokio::spawn(async move {
        let mut lines = stdout.lines();
        let mut token_sender = Some(token_tx);
        let mut ca_sender = Some(ca_tx);
        while let Some(line) = lines.next_line().await? {
            let mut token_parts = line.trim().split(':');
            let mut ca_parts = line.trim().split('=');
            if let Some(left) = ca_parts.next()
                && left == VAULT_CACERT_ENV_STRING
                && let Some(ca_sender) = ca_sender.take()
            {
                // Vault prints: $ export VAULT_CACERT='/path/to/cert'
                // Strip the surrounding single quotes that the shell export syntax includes.
                let raw = ca_parts.next().unwrap();
                let path = raw.trim_matches('\'').to_string();
                ca_sender.send(path).ok();
            }
            if let Some(left) = token_parts.next()
                && left == ROOT_TOKEN
                && let Some(token_sender) = token_sender.take()
            {
                token_sender
                    .send(token_parts.next().unwrap().to_string())
                    .ok();
            }
            // there's no logger so can't use tracing
            println!("{line}");
        }
        Ok::<(), eyre::Error>(())
    });

    tokio::spawn(async move {
        let mut lines = stderr.lines();
        while let Some(line) = lines.next_line().await? {
            // there's no logger so can't use tracing
            eprintln!("{line}");
        }
        Ok::<(), eyre::Error>(())
    });

    // Vault dev prints the token and CA cert on startup. If it loses the port
    // race it exits instead, dropping these senders -- which surfaces here as an
    // error so start() retries. The timeout only guards a genuine hang.
    let ready = async {
        let token = token_rx.await.context("waiting for vault token")?;
        let ca_cert = ca_rx.await.context("waiting for vault CA cert")?;
        Ok::<(String, String), eyre::Report>((token, ca_cert))
    };
    let (token, ca_cert) = tokio::time::timeout(STARTUP_TIMEOUT, ready)
        .await
        .context("timed out waiting for vault to report readiness")??;
    let ca_cert = if addr.is_ipv6() {
        LOCALHOST_CERTS.ca_cert.display().to_string()
    } else {
        ca_cert
    };

    // Vault announces the cert path in its stdout log before it finishes writing the
    // file to disk. Poll until the file is present so callers can use it immediately.
    let cert_ready_deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if Path::new(&ca_cert).exists() {
            break;
        }
        if std::time::Instant::now() >= cert_ready_deadline {
            eyre::bail!("vault CA cert never appeared at {ca_cert} after 10 seconds");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    Ok(Vault {
        addr,
        process,
        token,
        ca_cert,
        _tls_dir: tls_dir,
    })
}
