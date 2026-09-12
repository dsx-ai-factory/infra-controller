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
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bmc_mock::mac_address_pool::MacAddressPool;
use forge_tls::client_config::get_root_ca_path;
use futures::future::try_join_all;
use machine_a_tron::{
    BmcMockRegistry, DeviceHandle, DhcpClient, MachineATron, MachineATronConfig,
    MachineATronContext, UdpDhcpService, api_throttler, spawn_desired_firmware_refresher,
};
use rpc::forge_api_client::FailOverOn;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig, RetryConfig};
use rpc::protos::forge_api_client::ForgeApiClient;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

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

    // We want the API client to constantly switch between API servers if the test has more than one,
    // to emulate what a load balancer would do.
    let forge_api_client =
        ForgeApiClient::new_with_failover_behavior(&api_config, FailOverOn::EveryApiCall);

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

    // Same refresher as production main.rs, so integration suites exercise
    // live target changes; aborted in shutdown so it cannot outlive the helper.
    let firmware_refresher = spawn_desired_firmware_refresher(app_context.clone());

    let mat = MachineATron::new(app_context.clone());
    let simulators = mat.make_devices(false).await?;
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
            firmware_refresher,
            dhcp_service,
        },
    ))
}

pub struct MachineATronHandle {
    _stop_tx: oneshot::Sender<()>,
    _join_handle: JoinHandle<eyre::Result<()>>,
    firmware_refresher: JoinHandle<()>,
    dhcp_service: Option<UdpDhcpService>,
}

impl MachineATronHandle {
    pub async fn shutdown(mut self) -> eyre::Result<()> {
        drop(self._stop_tx);
        let mat_result = self._join_handle.await?;
        self.firmware_refresher.abort();
        if let Some(dhcp_service) = self.dhcp_service.take() {
            dhcp_service.shutdown().await?;
        }
        mat_result
    }
}
