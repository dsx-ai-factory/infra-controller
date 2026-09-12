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
use std::time::Duration;

use eyre::Context;
use http_body_util::Full;
use hyper::body::Bytes;
use hyper_util::rt::TokioExecutor;
use size::Size;
use ssh_console::config::Defaults;
use temp_dir::TempDir;
use tokio::net::TcpStream;

use crate::util::fixtures::{
    API_CA_CERT, API_CLIENT_CERT, API_CLIENT_KEY, AUTHORIZED_KEYS_PATH, SSH_HOST_KEY,
};

#[derive(Default)]
pub(crate) struct ConfigOverrides {
    pub(crate) reconnect_interval_base: Option<Duration>,
    pub(crate) reconnect_interval_max: Option<Duration>,
    pub(crate) successful_connection_minimum_duration: Option<Duration>,
    pub(crate) force_deactivate_conflicting_ipmi_sol_sessions: Option<bool>,
}

pub(crate) async fn spawn(
    carbide_port: u16,
    config_overrides: Option<ConfigOverrides>,
) -> eyre::Result<NewSshConsoleHandle> {
    let listen_address = "127.0.0.1:0".parse().expect("Invalid listen address");
    let metrics_address = "127.0.0.1:0".parse().expect("Invalid metrics address");
    let api_listen_address = "127.0.0.1:0".parse().expect("Invalid API listen address");

    let logs_dir = TempDir::new().context("error creating temp dir for console logs")?;

    let config = ssh_console::config::Config {
        listen_address,
        metrics_address,
        api_listen_address,
        api_allowed_client_spiffe_id: Defaults::api_allowed_client_spiffe_id(),
        carbide_uri: format!("https://localhost:{carbide_port}")
            .try_into()
            .expect("Invalid URI?"),
        authorized_keys_path: Some(AUTHORIZED_KEYS_PATH.clone()),
        host_key_path: SSH_HOST_KEY.clone(),
        override_bmcs: None,
        dpus: false,
        insecure: false,
        override_bmc_ssh_port: None,
        override_ipmi_port: Some(1623),
        insecure_ipmi_ciphers: true,
        force_deactivate_conflicting_ipmi_sol_sessions: config_overrides
            .as_ref()
            .and_then(|c| c.force_deactivate_conflicting_ipmi_sol_sessions)
            .unwrap_or(false),
        forge_root_ca_path: API_CA_CERT.clone(),
        client_cert_path: API_CLIENT_CERT.clone(),
        client_key_path: API_CLIENT_KEY.clone(),
        openssh_certificate_ca_fingerprints: vec![],
        admin_certificate_role: None,
        api_poll_interval: Duration::from_secs(1),
        console_logging_enabled: true,
        console_logs_path: logs_dir.path().to_path_buf(),
        override_bmc_ssh_host: None,
        // Eagerly retry if the connection was only open a short while (needed for tests to avoid
        // long backoff intervals.)
        reconnect_interval_base: config_overrides
            .as_ref()
            .and_then(|c| c.reconnect_interval_base)
            .unwrap_or(Defaults::reconnect_interval_base()),
        reconnect_interval_max: config_overrides
            .as_ref()
            .and_then(|c| c.reconnect_interval_max)
            .unwrap_or(Defaults::reconnect_interval_max()),
        successful_connection_minimum_duration: config_overrides
            .as_ref()
            .and_then(|c| c.successful_connection_minimum_duration)
            .unwrap_or(Duration::ZERO),
        log_rotate_max_rotated_files: 3,
        log_rotate_max_size: Size::from_kib(10),
        hosts: true,
        openssh_certificate_authorization: ssh_console::config::Defaults::cert_authorization(),
    };

    let spawn_handle = ssh_console::spawn(config).await?;
    let listen_address = spawn_handle.listen_address();
    let metrics_address = spawn_handle.metrics_address();
    assert_ne!(listen_address.port(), 0);
    assert_ne!(metrics_address.port(), 0);
    TcpStream::connect(listen_address)
        .await
        .context("error connecting to runtime-assigned SSH address")?;
    TcpStream::connect(metrics_address)
        .await
        .context("error connecting to runtime-assigned metrics address")?;

    Ok(NewSshConsoleHandle {
        addr: listen_address,
        metrics_address,
        // Make sure the logs dir doesn't drop.
        logs_dir,
        spawn_handle,
    })
}

pub(crate) struct NewSshConsoleHandle {
    pub(crate) addr: SocketAddr,
    pub(crate) metrics_address: SocketAddr,
    pub(crate) logs_dir: TempDir,
    pub(crate) spawn_handle: ssh_console::SpawnHandle,
}

pub(crate) async fn get_metrics(addr: SocketAddr) -> eyre::Result<String> {
    use http_body_util::BodyExt;
    String::from_utf8_lossy(
        hyper_util::client::legacy::Builder::new(TokioExecutor::new())
            .build_http::<Full<Bytes>>()
            .get(format!("http://{addr}/metrics").try_into().unwrap())
            .await
            .context("error fetching metrics")?
            .into_body()
            .collect()
            .await
            .context("error fetching metrics body")?
            .to_bytes()
            .as_ref(),
    )
    .parse()
    .context("error parsing prometheus metrics")
}
