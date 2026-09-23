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
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tower_http::normalize_path::NormalizePathLayer;

use crate::combined_router;

mod progress;
mod transport;

pub(crate) use transport::OutputStallTimeout;

pub enum ListenerOrAddress {
    Listener(TcpListener),
    Address(SocketAddr),
}

impl ListenerOrAddress {
    pub fn address(&self) -> std::io::Result<SocketAddr> {
        match self {
            Self::Listener(l) => l.local_addr(),
            Self::Address(a) => Ok(*a),
        }
    }
}

/// Multiplexed HTTP routers on a single IP/port.
///
/// HTTP header `forwarded` is used to route the request to the
/// appropriate entry.
///
/// Note: that this code is not BMC-mock specific and potentially can
/// be separate crate if needed.
#[derive(Debug)]
pub struct CombinedServer {
    join_handle: Option<JoinHandle<std::io::Result<()>>>,
    axum_handle: axum_server::Handle<SocketAddr>,
    pub address: SocketAddr,
}

impl Drop for CombinedServer {
    fn drop(&mut self) {
        if let Some(join_handle) = self.join_handle.take()
            && !join_handle.is_finished()
        {
            tracing::info!(
                listen_address = %self.address,
                "Stopping BMC mock",
            );
            self.axum_handle.shutdown();
            join_handle.abort()
        }
    }
}

impl CombinedServer {
    pub fn run(
        name: &str,
        routers_by_ip_address: Arc<RwLock<HashMap<String, Router>>>,
        listener_or_address: Option<ListenerOrAddress>,
        server_config: rustls::ServerConfig,
    ) -> Self {
        Self::run_router(
            name,
            combined_router(routers_by_ip_address),
            listener_or_address,
            server_config,
        )
    }

    /// Serve a router over TLS. A response carrying an [`OutputStallTimeout`]
    /// extension has its body bounded: if an emitted frame is not followed by
    /// transport progress within that duration, the whole connection is closed.
    pub fn run_router(
        name: &str,
        router: Router,
        listener_or_address: Option<ListenerOrAddress>,
        server_config: rustls::ServerConfig,
    ) -> Self {
        let config = RustlsConfig::from_config(Arc::new(server_config));

        let axum_handle = axum_server::Handle::new();

        let (addr, server) = match listener_or_address {
            Some(ListenerOrAddress::Address(addr)) => (
                addr,
                axum_server::bind_rustls(addr, config).handle(axum_handle.clone()),
            ),
            Some(ListenerOrAddress::Listener(listener)) => {
                listener.set_nonblocking(true).ok();
                (
                    listener.local_addr().unwrap(),
                    // Note: This only fails if the listener is not configured as non-blocking. If
                    // we couldn't configure it as such, it was likely in use.
                    axum_server::from_tcp_rustls(listener, config).expect("BUG: Failure confguring rustls listner: Socket could not be configured as nonblocking. Maybe already in use?").handle(axum_handle.clone()),
                )
            }
            None => {
                let addr = SocketAddr::from(([0, 0, 0, 0], 1266));
                (
                    addr,
                    axum_server::bind_rustls(addr, config).handle(axum_handle.clone()),
                )
            }
        };
        let server = server.map(transport::Acceptor::new);
        tracing::info!(
            listen_address = %addr,
            "BMC mock listening",
        );

        // Inject middleware to normalize request URIs by dropping the trailing slash
        let router = router.layer(NormalizePathLayer::trim_trailing_slash());
        let join_handle = tokio::task::Builder::new()
            .name(name)
            .spawn(async move {
                server
                    .serve(router.into_make_service())
                    .await
                    .inspect_err(|e| {
                        tracing::error!(
                            listen_address = %addr,
                            error = %e,
                            "BMC mock could not listen",
                        )
                    })?;
                Ok(())
            })
            .expect("tokio spawn error");
        Self {
            axum_handle,
            join_handle: Some(join_handle),
            address: addr,
        }
    }

    pub async fn stop(&mut self) -> std::io::Result<()> {
        if let Some(join_handle) = self.join_handle.take() {
            self.axum_handle.shutdown();
            join_handle.await.expect("join error")
        } else {
            Ok(())
        }
    }

    pub async fn wait(&mut self) -> std::io::Result<()> {
        if let Some(join_handle) = self.join_handle.take() {
            join_handle.await.expect("join error")
        } else {
            Ok(())
        }
    }
}
