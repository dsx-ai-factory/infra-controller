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
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use bmc_mock::mac_address_pool::MacAddressPool;
use carbide_uuid::machine::MachineId;
use eyre::WrapErr;
use forge_tls::client_config::get_root_ca_path;
use futures::future::try_join_all;
use machine_a_tron::{
    BmcMockRegistry, ControlState, DeviceHandle, DeviceStatusConfig, DhcpClient, DpuMachineHandle,
    MachineATron, MachineATronConfig, MachineATronContext, SimulatorRegistry, UdpDhcpService,
};
// Expose the service and its router together so callers can retain one service
// while mounting its wire API and observing that same instance.
pub use rms_mock::{RmsMock, router as rms_service_router};
use rpc::forge::MachinesByIdsRequest;
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

/// `wait_for_host_state` waits until the host is locally up and Core reports
/// exactly `target_state`.
///
/// Sleeps 500 ms between observations, only while this future is awaited. Missing
/// identity or a missing Core record is pending startup. RPC errors, stopped actors,
/// and unsupported device kinds fail the wait. `timeout` bounds the whole wait,
/// including RPCs, and expiry reports the last local and API observations.
pub async fn wait_for_host_state(
    addrs: &[SocketAddr],
    host: &DeviceHandle,
    target_state: &str,
    timeout: Duration,
) -> eyre::Result<()> {
    wait_for_state(
        || Ok((host.observed_machine_id(), host.is_up()?)),
        |id| machine_state(addrs, id),
        target_state,
        timeout,
    )
    .await
    .wrap_err_with(|| format!("waiting for host {} to reach {target_state}", host.mat_id()))
}

/// `wait_for_dpu_state` waits for this DPU's own Core state and local-up condition,
/// with the same exact matching, error handling and deadline as
/// [`wait_for_host_state`].
pub async fn wait_for_dpu_state(
    addrs: &[SocketAddr],
    dpu: &DpuMachineHandle,
    target_state: &str,
    timeout: Duration,
) -> eyre::Result<()> {
    wait_for_state(
        || Ok((dpu.observed_machine_id(), dpu.is_up()?)),
        |id| machine_state(addrs, id),
        target_state,
        timeout,
    )
    .await
    .wrap_err_with(|| format!("waiting for DPU {} to reach {target_state}", dpu.mat_id()))
}

async fn wait_for_state<Fut>(
    local_state: impl Fn() -> eyre::Result<(Option<MachineId>, bool)>,
    mut read_state: impl FnMut(MachineId) -> Fut,
    target_state: &str,
    timeout: Duration,
) -> eyre::Result<()>
where
    Fut: Future<Output = eyre::Result<Option<String>>>,
{
    let mut last_id = None;
    let mut last_up = false;
    let mut last_api_id = None;
    let mut last_state = None;
    tokio::time::timeout(timeout, async {
        loop {
            (last_id, last_up) = local_state()?;
            if let Some(machine_id) = last_id {
                last_state = read_state(machine_id)
                    .await
                    .wrap_err_with(|| format!("waiting for {machine_id} to reach {target_state}"))?;
                last_api_id = Some(machine_id);
                // Recheck local state after the RPC: a reboot can change it while Core responds.
                (last_id, last_up) = local_state()?;
                if last_id == Some(machine_id)
                    && last_up
                    && last_state.as_deref() == Some(target_state)
                {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| {
        eyre::eyre!(
            "machine did not reach {target_state} within {timeout:?}: id={last_id:?}, local_up={last_up}, API id={last_api_id:?}, API state={last_state:?}"
        )
    })?
}

async fn machine_state(
    addrs: &[SocketAddr],
    machine_id: MachineId,
) -> eyre::Result<Option<String>> {
    let request = MachinesByIdsRequest {
        machine_ids: vec![machine_id],
        include_history: false,
    };
    let response = crate::api_client::call(addrs, "FindMachinesByIds", |mut client| async move {
        client.find_machines_by_ids(request).await
    })
    .await?;
    Ok(response
        .machines
        .into_iter()
        .find(|machine| machine.id == Some(machine_id))
        .map(|machine| machine.state))
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
            // All simulators share a client quota; avoid a rack-sized burst of API deletes.
            for simulator in device_simulators {
                let api_client = app_context.api_client();
                simulator.delete_from_api(api_client).await?;
            }
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::future::{pending, ready};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn state_wait_requires_registration_exact_state_and_local_up() {
        let machine_id: MachineId = "fm100dsq7h9eabr4qrfh88vipv7il5sbpfgfq6lhb30n36offacbmusfb50"
            .parse()
            .unwrap();
        // Each first observation fails one condition while the others permit
        // success. The next poll supplies all three, so no condition can mask another.
        for (name, initial_id, initial_up, initial_state) in [
            ("unregistered", None, true, Some("Ready")),
            ("missing record", Some(machine_id), true, None),
            (
                "non-exact state",
                Some(machine_id),
                true,
                Some("Assigned/Ready"),
            ),
            ("locally down", Some(machine_id), false, Some("Ready")),
        ] {
            let started = tokio::time::Instant::now();
            let api_reads = Cell::new(0);
            let local_reads = Cell::new(0);
            let initial_local_reads = if initial_id.is_some() { 2 } else { 1 };
            wait_for_state(
                || {
                    local_reads.set(local_reads.get() + 1);
                    if local_reads.get() <= initial_local_reads {
                        Ok((initial_id, initial_up))
                    } else {
                        Ok((Some(machine_id), true))
                    }
                },
                |id| {
                    assert_eq!(id, machine_id, "{name}");
                    api_reads.set(api_reads.get() + 1);
                    let state = if api_reads.get() == 1 {
                        initial_state
                    } else {
                        Some("Ready")
                    };
                    ready(Ok(state.map(str::to_owned)))
                },
                "Ready",
                Duration::from_secs(5),
            )
            .await
            .unwrap();

            assert_eq!(started.elapsed(), Duration::from_millis(500), "{name}");
            assert_eq!(
                api_reads.get(),
                if initial_id.is_some() { 2 } else { 1 },
                "{name}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn state_wait_rechecks_local_identity_and_up_after_rpc() {
        let machine_id: MachineId = "fm100dsq7h9eabr4qrfh88vipv7il5sbpfgfq6lhb30n36offacbmusfb50"
            .parse()
            .unwrap();
        let replacement_id: MachineId =
            "fm100ds00vmuk40pi57g1trrdvfs7e4j6bo4r3s451d67oseree51b49bog"
                .parse()
                .unwrap();
        // A successful response must not establish readiness using observations
        // from before a local reboot or identity replacement.
        for (name, after_rpc) in [
            ("identity replaced", (Some(replacement_id), true)),
            ("locally down", (Some(machine_id), false)),
        ] {
            let local = Cell::new((Some(machine_id), true));
            let api_reads = Cell::new(0);
            let started = tokio::time::Instant::now();
            wait_for_state(
                || Ok(local.get()),
                |id| {
                    api_reads.set(api_reads.get() + 1);
                    if api_reads.get() == 1 {
                        assert_eq!(id, machine_id, "{name}");
                        local.set(after_rpc);
                    } else {
                        assert_eq!(Some(id), after_rpc.0, "{name}");
                        local.set((Some(id), true));
                    }
                    ready(Ok(Some("Ready".to_owned())))
                },
                "Ready",
                Duration::from_secs(5),
            )
            .await
            .unwrap();

            assert_eq!(api_reads.get(), 2, "{name}");
            assert_eq!(started.elapsed(), Duration::from_millis(500), "{name}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn state_wait_deadline_includes_inflight_read() {
        let machine_id: MachineId = "fm100dsq7h9eabr4qrfh88vipv7il5sbpfgfq6lhb30n36offacbmusfb50"
            .parse()
            .unwrap();
        let started = tokio::time::Instant::now();
        let error = wait_for_state(
            || Ok((Some(machine_id), true)),
            |_| pending(),
            "Ready",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert_eq!(started.elapsed(), Duration::from_secs(1));
        assert!(error.to_string().contains("did not reach Ready"));
        assert!(error.to_string().contains(&machine_id.to_string()));
    }

    #[tokio::test(start_paused = true)]
    async fn state_wait_deadline_bounds_repeated_nonmatching_reads() {
        let machine_id: MachineId = "fm100dsq7h9eabr4qrfh88vipv7il5sbpfgfq6lhb30n36offacbmusfb50"
            .parse()
            .unwrap();
        let started = tokio::time::Instant::now();
        let error = wait_for_state(
            || Ok((Some(machine_id), true)),
            |_| ready(Ok(Some("Assigned/Ready".to_owned()))),
            "Ready",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert_eq!(started.elapsed(), Duration::from_secs(1));
        let message = error.to_string();
        assert!(message.contains("did not reach Ready"));
        assert!(message.contains(&machine_id.to_string()));
        assert!(message.contains("local_up=true"));
        assert!(message.contains("Assigned/Ready"));
    }

    #[tokio::test(start_paused = true)]
    async fn state_wait_preserves_local_failure_before_and_after_rpc() {
        let machine_id: MachineId = "fm100dsq7h9eabr4qrfh88vipv7il5sbpfgfq6lhb30n36offacbmusfb50"
            .parse()
            .unwrap();
        // An actor may stop before polling or while Core answers Ready. Neither
        // case should consume the deadline or accept that stale Ready response.
        for (name, failing_read, expected_api_reads) in [("before RPC", 1, 0), ("during RPC", 2, 1)]
        {
            let local_reads = Cell::new(0);
            let api_reads = Cell::new(0);
            let started = tokio::time::Instant::now();
            let error = wait_for_state(
                || {
                    local_reads.set(local_reads.get() + 1);
                    eyre::ensure!(local_reads.get() != failing_read, "injected stopped actor");
                    Ok((Some(machine_id), true))
                },
                |_| {
                    api_reads.set(api_reads.get() + 1);
                    ready(Ok(Some("Ready".to_owned())))
                },
                "Ready",
                Duration::from_secs(90),
            )
            .await
            .unwrap_err();

            assert!(
                error.to_string().contains("injected stopped actor"),
                "{name}"
            );
            assert!(started.elapsed().is_zero(), "{name}");
            assert_eq!(api_reads.get(), expected_api_reads, "{name}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn state_wait_preserves_rpc_failure() {
        let machine_id: MachineId = "fm100dsq7h9eabr4qrfh88vipv7il5sbpfgfq6lhb30n36offacbmusfb50"
            .parse()
            .unwrap();
        let started = tokio::time::Instant::now();
        let api_reads = Cell::new(0);
        let error = wait_for_state(
            || Ok((Some(machine_id), true)),
            |_| {
                api_reads.set(api_reads.get() + 1);
                ready(Err(eyre::eyre!("injected RPC failure")))
            },
            "Ready",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert_eq!(api_reads.get(), 1);
        assert!(started.elapsed().is_zero());
        let message = format!("{error:?}");
        assert!(message.contains("injected RPC failure"));
        assert!(message.contains(&machine_id.to_string()));
        assert!(message.contains("reach Ready"));
    }
}
