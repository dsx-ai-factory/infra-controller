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
    BmcMockRegistry, ControlState, DeviceHandle, DeviceStatusConfig, DhcpClient, MachineATron,
    MachineATronConfig, MachineATronContext, SimulatorRegistry, UdpDhcpService, api_throttler,
};
// Expose the service and its router together so callers can retain one service
// while mounting its wire API and observing that same instance.
pub use rms_mock::{RmsMock, router as rms_service_router};
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

/// Starts machine-a-tron and returns its provisionable devices and shutdown handle.
///
/// Simulators keep running until the caller shuts down or drops the returned handle.
/// Callers must bound their waits for simulated machines to reach the expected API state.
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

    let (stop_tx, stop_rx) = oneshot::channel::<bool>();
    let device_simulators = simulators.devices().to_vec();
    let join_handle = tokio::spawn(async move {
        // Dropping the handle retains the normal cleanup behavior.
        let delete_from_api = stop_rx.await.unwrap_or(true);

        try_join_all(
            device_simulators
                .iter()
                .map(|simulator| simulator.shutdown()),
        )
        .await?;

        if delete_from_api {
            try_join_all(device_simulators.into_iter().map(|simulator| {
                let api_client = app_context.api_client();
                async move { simulator.delete_from_api(api_client).await }
            }))
            .await?;
        }

        Ok(())
    });

    Ok((
        provisionable_handles,
        MachineATronHandle {
            _stop_tx: stop_tx,
            _join_handle: join_handle,
            dhcp_service,
            registry: simulators,
        },
    ))
}

/// Owns a running simulator group and controls its shutdown and API cleanup.
pub struct MachineATronHandle {
    _stop_tx: oneshot::Sender<bool>,
    _join_handle: JoinHandle<eyre::Result<()>>,
    dhcp_service: Option<UdpDhcpService>,
    registry: SimulatorRegistry,
}

impl MachineATronHandle {
    /// All running devices, including switches and power shelves.
    ///
    /// Host handles expose their DPU children through [`DeviceHandle::dpus`].
    pub fn registry(&self) -> &SimulatorRegistry {
        &self.registry
    }

    /// Builds an RMS service over this exact registry for wire calls and observation.
    ///
    /// Retain the returned service across rebuilds to preserve completed jobs and
    /// reset generations. `redfish_port` is the simulated BMC listener's port.
    pub fn rms_service(&self, config: rms_mock::RmsMockConfig, redfish_port: u16) -> Arc<RmsMock> {
        let control_state = ControlState::new(
            self.registry.clone(),
            DeviceStatusConfig::new(redfish_port),
            "integration-test".into(),
        );
        Arc::new(RmsMock::new(Arc::new(control_state), config))
    }

    /// Stops the simulators and deletes their machines through the running API.
    pub async fn shutdown(self) -> eyre::Result<()> {
        self.shutdown_with_api_cleanup(true).await
    }

    /// Stops the simulators after API shutdown, leaving database cleanup to the caller.
    pub async fn shutdown_without_api_cleanup(self) -> eyre::Result<()> {
        self.shutdown_with_api_cleanup(false).await
    }

    async fn shutdown_with_api_cleanup(mut self, delete_from_api: bool) -> eyre::Result<()> {
        self._stop_tx.send(delete_from_api).ok();
        let mat_result = self._join_handle.await;
        if let Some(dhcp_service) = self.dhcp_service.take() {
            dhcp_service.shutdown().await?;
        }
        mat_result?
    }
}
