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
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use bmc_mock::mac_address_pool::MacAddressPool;
use forge_tls::client_config::get_root_ca_path;
use futures::future::try_join_all;
use machine_a_tron::{
    BmcMockRegistry, DeviceHandle, DhcpClient, MachineATron, MachineATronConfig,
    MachineATronContext, UdpDhcpService, api_throttler,
};
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig, ForgeClientT, RetryConfig};
use rpc::protos::forge_api_client::ForgeApiClient;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Emulate a load balancer while retaining each server's connection.
#[derive(Debug)]
struct RoundRobinConnectionProvider {
    clients: Vec<ForgeApiClient>,
    next_client: AtomicUsize,
}

#[async_trait::async_trait]
impl tonic_client_wrapper::ConnectionProvider<ForgeClientT> for RoundRobinConnectionProvider {
    async fn provide_connection(&self) -> Result<ForgeClientT, tonic::Status> {
        let index = self.next_client.fetch_add(1, Ordering::Relaxed) % self.clients.len();
        let client = &self.clients[index];
        tracing::debug!(url = client.url(), "Selecting MAT test API server");
        client.connection().await
    }

    async fn connection_is_stale(&self, _last_connected: SystemTime) -> bool {
        // Select another server for each RPC; its client handles connection reuse and cert renewal.
        true
    }

    fn connection_url(&self) -> &str {
        self.clients[0].url()
    }
}

/// Run a machine-a-tron instance with the given config in the background, returning a JoinHandle
/// that can be waited on.
///
/// The background job will continually run [HostMachine::process_state] on each machine until each
/// of them reaches a `Ready` state, then it will return. Callers are responsible for configuring a
/// timeout in case a ready state is not reached.
pub async fn run_local(
    app_config: MachineATronConfig,
    additional_api_urls: Vec<String>,
    repo_root: &Path,
    bmc_address_registry: BmcMockRegistry,
    mac_address_pool: Arc<Mutex<MacAddressPool>>,
) -> eyre::Result<(Vec<DeviceHandle>, MachineATronHandle)> {
    app_config.validate()?;

    let forge_root_ca_path = get_root_ca_path(None, None); // Will get it from the local repo
    let mut forge_client_config = ForgeClientConfig::new(forge_root_ca_path.clone(), None);
    forge_client_config.suppress_insecure_tls_warning = true;

    let api_config = ApiConfig::new_with_multiple_urls(
        &app_config.carbide_api_url,
        &additional_api_urls,
        &forge_client_config,
        RetryConfig {
            retries: 10,
            interval: Duration::from_secs(1),
        },
    );

    // Switch servers on each RPC without rebuilding their TLS connections.
    let forge_api_client = if additional_api_urls.is_empty() {
        ForgeApiClient::new(&api_config)
    } else {
        ForgeApiClient::build(RoundRobinConnectionProvider {
            clients: std::iter::once(&app_config.carbide_api_url)
                .chain(additional_api_urls.iter())
                .map(|url| {
                    ForgeApiClient::new(
                        &ApiConfig::new(url, &forge_client_config)
                            .with_retry_config(api_config.retry_config),
                    )
                })
                .collect(),
            next_client: AtomicUsize::new(0),
        })
    };

    let api_throttler = api_throttler::run(
        tokio::time::interval(Duration::from_secs(2)),
        forge_api_client.clone().into(),
    );

    let desired_firmware = forge_api_client
        .get_desired_firmware_versions()
        .await?
        .entries;

    tracing::info!(
        ?desired_firmware,
        "Got desired firmware versions from the server",
    );

    let (dhcp_client, dhcp_service) =
        DhcpClient::start(&app_config, forge_api_client.clone().into()).await?;

    let app_context = Arc::new(MachineATronContext {
        bmc_registry: bmc_address_registry.clone(),
        app_config,
        forge_client_config,
        bmc_mock_certs_dir: Some(repo_root.join("crates/bmc-mock")),
        api_throttler,
        desired_firmware_versions: std::sync::RwLock::new(desired_firmware),
        forge_api_client,
        dhcp_client,
        mac_address_pool,
    });

    let mat = MachineATron::new(app_context.clone());
    let (simulators, _) = mat.make_devices(false).await?;
    let provisionable_handles = simulators.provisionable_handles();

    let (stop_tx, stop_rx) = oneshot::channel();
    let device_simulators = simulators.devices().to_vec();
    let join_handle = tokio::spawn(async move {
        stop_rx.await.ok(); // this finishes when stop_tx is dropped

        try_join_all(
            device_simulators
                .iter()
                .map(|simulator| simulator.shutdown()),
        )
        .await?;

        try_join_all(device_simulators.into_iter().map(|simulator| {
            let api_client = app_context.api_client();
            async move { simulator.delete_from_api(api_client).await }
        }))
        .await?;

        Ok(())
    });

    Ok((
        provisionable_handles,
        MachineATronHandle {
            _stop_tx: stop_tx,
            _join_handle: join_handle,
            dhcp_service,
        },
    ))
}

pub struct MachineATronHandle {
    _stop_tx: oneshot::Sender<()>,
    _join_handle: JoinHandle<eyre::Result<()>>,
    dhcp_service: Option<UdpDhcpService>,
}

impl MachineATronHandle {
    pub async fn shutdown(mut self) -> eyre::Result<()> {
        drop(self._stop_tx);
        let mat_result = self._join_handle.await?;
        if let Some(dhcp_service) = self.dhcp_service.take() {
            dhcp_service.shutdown().await?;
        }
        mat_result
    }
}
