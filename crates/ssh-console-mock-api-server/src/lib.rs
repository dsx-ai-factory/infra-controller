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
mod api;
#[allow(dead_code)]
mod generated;

use std::fs;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use api_test_helper::utils::LOCALHOST_CERTS;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Identity, Server, ServerTlsConfig};
use uuid::Uuid;

use crate::generated::forge::forge_server::ForgeServer;
use crate::generated::forge::{self};
use crate::generated::{common, machine_discovery};

#[derive(Debug, Clone)]
pub struct MockHost {
    pub machine_id: carbide_uuid::machine::MachineId,
    pub instance_id: Uuid,
    pub tenant_public_key: String,
    pub sys_vendor: &'static str,
    pub bmc_ip: IpAddr,
    pub serial_console_ssh_port: Option<u16>,
    pub ipmi_port: Option<u16>,
    pub bmc_user: String,
    pub bmc_password: String,
}

#[allow(deprecated)]
impl From<MockHost> for forge::Machine {
    fn from(value: MockHost) -> Self {
        let discovery_info = Some(machine_discovery::DiscoveryInfo {
            dmi_data: Some(machine_discovery::DmiData {
                sys_vendor: value.sys_vendor.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        });
        Self {
            id: Some(value.machine_id),
            // Deprecated flat field kept for backwards-compat with callers that haven't
            // migrated to status.discovery_info yet.
            discovery_info: discovery_info.clone(),
            status: Some(forge::MachineStatus {
                discovery_info,
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

impl TryFrom<MockHost> for forge::Instance {
    type Error = carbide_uuid::machine::InvalidMachineType;

    fn try_from(value: MockHost) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Some(common::InstanceId {
                value: value.instance_id.to_string(),
            }),
            machine_id: Some(value.machine_id.try_into()?),
            ..Default::default()
        })
    }
}

#[derive(Debug)]
pub struct MockApiServer {
    pub mock_hosts: Arc<Vec<MockHost>>,
}

pub struct MockApiServerHandle {
    pub addr: SocketAddr,
    _shutdown_tx: oneshot::Sender<()>,
}

impl MockApiServer {
    pub async fn spawn(self) -> eyre::Result<MockApiServerHandle> {
        let cert = fs::read(&LOCALHOST_CERTS.server_cert)?;
        let key = fs::read(&LOCALHOST_CERTS.server_key)?;
        let identity = Identity::from_pem(cert, key);
        let tls = ServerTlsConfig::new().identity(identity);
        rustls::crypto::aws_lc_rs::default_provider()
            .install_default()
            .inspect_err(|crypto_provider| {
                tracing::warn!(?crypto_provider, "Crypto provider already configured")
            })
            .ok(); // if something else is already default, ignore.

        // Serve on the listener we just bound, so its port is held continuously and no other
        // concurrent test can claim it before the server starts accepting.
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener
            .local_addr()?
            .to_socket_addrs()?
            .next()
            .expect("No socket available");

        println!("Mock gRPC server listening on {addr}");

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(
            Server::builder()
                .tls_config(tls)?
                .add_service(ForgeServer::new(self))
                .serve_with_incoming_shutdown(TcpIncoming::from(listener), async move {
                    shutdown_rx.await.ok();
                }),
        );

        Ok(MockApiServerHandle {
            addr,
            _shutdown_tx: shutdown_tx,
        })
    }
}
