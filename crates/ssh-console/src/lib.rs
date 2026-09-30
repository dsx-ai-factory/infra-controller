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

mod api_server;
mod bmc;
mod io_util;
mod metrics;
mod ssh_cert_parsing;
mod ssh_server;
mod tcp_listener;

mod console_logger;
mod frontend;

// pub mods are only ones used by main.rs and integration tests
pub mod config;
pub mod shutdown_handle;

// Used by fuzz tests
use std::sync::Arc;

pub use bmc::vendor::{EscapeSequence, IPMITOOL_ESCAPE_SEQUENCE};
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::config::Config;
use crate::metrics::MetricsState;
use crate::shutdown_handle::{ReadyHandle, ShutdownHandle};

pub static POWER_RESET_COMMAND: &str = "power reset";

/// Run a ssh-console server in the background, returning a [`SpawnHandle`] once the service is
/// healthy and ready. When the handle is dropped, the server will exit.
pub async fn spawn(config: Config) -> Result<SpawnHandle, SpawnError> {
    let config = Arc::new(config);
    let metrics = Arc::new(MetricsState::new());
    let forge_api_client = config.make_forge_api_client();

    let (cancel_token, drop_guard) = {
        let t = CancellationToken::new();
        (t.clone(), t.drop_guard())
    };

    // 1) Start BMC client pool
    let mut bmc_client_pool = bmc::client_pool::spawn(
        config.clone(),
        forge_api_client.clone(),
        &metrics.meter,
        cancel_token.clone(),
    );
    bmc_client_pool
        .wait_until_ready()
        .await
        .map_err(|_| SpawnError::ClientPoolUnknownFailure)?;

    // 2) Start SSH server itself
    let server = ssh_server::spawn(
        config.clone(),
        forge_api_client.clone(),
        bmc_client_pool.connection_store(),
        &metrics.meter,
        cancel_token.clone(),
    )
    .await?;

    // 3) Start the private console-log gRPC API.
    let api_server = api_server::spawn(
        config.clone(),
        bmc_client_pool.connection_store(),
        cancel_token.clone(),
    )
    .await?;

    // 4) Start metrics server
    let metrics_handle = metrics::spawn(config.clone(), metrics, cancel_token.clone()).await?;
    let listen_address = server.listen_address();
    let metrics_address = metrics_handle.metrics_address();
    let api_listen_address = api_server.listen_address();

    // 5) Wait for a shutdown signal, then shut down the above
    let join_handle = tokio::spawn(async move {
        cancel_token.cancelled().await;
        api_server.shutdown_and_wait().await;
        metrics_handle.shutdown_and_wait().await;
        bmc_client_pool.shutdown_and_wait().await;
        server.shutdown_and_wait().await;
    });

    Ok(SpawnHandle {
        listen_address,
        metrics_address,
        api_listen_address,
        drop_guard,
        join_handle,
    })
}

#[derive(thiserror::Error, Debug)]
pub enum SpawnError {
    #[error("unknown failure spawning BMC client pool")]
    ClientPoolUnknownFailure,
    #[error("error spawning SSH server: {0}")]
    SshServerSpawn(#[from] ssh_server::SpawnError),
    #[error("error spawning metrics server: {0}")]
    MetricsSpawn(#[from] metrics::SpawnError),
    #[error("error spawning private API server: {0}")]
    ApiServerSpawn(#[from] api_server::SpawnError),
}

pub struct SpawnHandle {
    listen_address: std::net::SocketAddr,
    metrics_address: std::net::SocketAddr,
    api_listen_address: std::net::SocketAddr,
    drop_guard: DropGuard,
    join_handle: JoinHandle<()>,
}

impl SpawnHandle {
    pub fn listen_address(&self) -> std::net::SocketAddr {
        self.listen_address
    }

    pub fn metrics_address(&self) -> std::net::SocketAddr {
        self.metrics_address
    }

    pub fn api_listen_address(&self) -> std::net::SocketAddr {
        self.api_listen_address
    }
}

impl ShutdownHandle<()> for SpawnHandle {
    fn into_parts(self) -> (DropGuard, JoinHandle<()>) {
        (self.drop_guard, self.join_handle)
    }
}

/// Helper for tasks where we use a child token of the passed-in CancellationToken, and also store a
/// DropGuard for it. That way, we can explicitly cancel just these tasks if the ClientHandle we're
/// returning is ever dropped, but still also cancel if the global CancellationToken is cancelled.
pub(crate) fn fork_cancel_token(cancel_token: CancellationToken) -> (CancellationToken, DropGuard) {
    let child = cancel_token.child_token();
    (child.clone(), child.drop_guard())
}
