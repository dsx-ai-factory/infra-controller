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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

mod logging;
mod ufm_mock;

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bmc_mock::mac_address_pool::{
    Config as MacAddressConfig, MacAddressPool, PoolConfig as MacAddressPoolConfig,
    RangesConfig as MacAddressRangesConfig,
};
use bmc_mock::{CombinedServer, ListenerOrAddress};
use clap::Parser;
use figment::Figment;
use figment::providers::{Format, Toml};
use forge_tls::client_config::{
    get_client_cert_info, get_config_from_file, get_proxy_info, get_root_ca_path,
};
use mac_address::MacAddress;
use machine_a_tron::{
    BmcMockRegistry, ControlState, DeviceStatusConfig, DhcpClient, MachineATron, MachineATronArgs,
    MachineATronConfig, MachineATronContext, SimulatorLifecycle, api_throttler,
    append_control_routes,
};
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use rpc::protos::forge_api_client::ForgeApiClient;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::logging::init_logging;
use crate::ufm_mock::HostedUfmMock;

#[tokio::main(flavor = "multi_thread", worker_threads = 32)]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = MachineATronArgs::parse();
    let config_path = Path::new(&args.config_file);
    if !config_path.is_file() {
        Err(format!("config: {} is not file", args.config_file.as_str()))?;
    }
    let fig = Figment::new().merge(Toml::file(config_path));
    let app_config: MachineATronConfig = fig.extract()?;
    app_config.validate()?;
    let ufm_config = app_config.ufm_mock.clone();

    init_logging(app_config.log_format, app_config.log_file.as_deref())?;

    let file_config = get_config_from_file();

    let forge_root_ca_path = get_root_ca_path(args.forge_root_ca_path, file_config.as_ref());
    let forge_client_cert = get_client_cert_info(
        args.client_cert_path,
        args.client_key_path,
        file_config.as_ref(),
    );
    let proxy = get_proxy_info().inspect_err(|e| {
        tracing::error!(
            error = %e,
            "Failed to get proxy info",
        )
    })?;

    let mut forge_client_config =
        ForgeClientConfig::new(forge_root_ca_path.clone(), Some(forge_client_cert));
    forge_client_config.socks_proxy(proxy);
    forge_client_config.connect_retries_max = Some(60);
    forge_client_config.connect_retries_interval = Some(Duration::from_secs(1));

    let bmc_registry = BmcMockRegistry::default();

    let api_config = ApiConfig::new(&app_config.carbide_api_url, &forge_client_config);

    let forge_api_client = ForgeApiClient::new(&api_config);
    let (dhcp_client, dhcp_service) =
        DhcpClient::start(&app_config, forge_api_client.clone().into()).await?;

    let api_throttler = api_throttler::run(
        tokio::time::interval(Duration::from_secs(2)),
        forge_api_client.clone().into(),
    );

    let desired_firmware_versions = forge_api_client
        .get_desired_firmware_versions()
        .await?
        .entries;

    tracing::info!(
        desired_firmware_versions = ?desired_firmware_versions,
        "Got desired firmware versions from the server",
    );

    let bmc_mock_port = app_config.bmc_mock_port;
    let hw_mac_address_ranges = app_config
        .hw_mac_address_ranges
        .as_ref()
        .map(|config| {
            MacAddressRangesConfig::new(config.base, config.host_bits, config.range_host_bits)
        })
        .unwrap_or_else(|| {
            MacAddressRangesConfig::new(MacAddress::new([6, 0, 0, 0, 0, 0]), 32, 8)
        })?;
    let inventory_id = format!(
        "mat-{}",
        hw_mac_address_ranges
            .base()
            .to_string()
            .to_ascii_lowercase()
    );

    let mac_address_pool = MacAddressPool::new(MacAddressConfig {
        pool: Some(
            app_config
                .mac_address_pool
                .as_ref()
                .map(|pool| MacAddressPoolConfig::new(pool.base, pool.host_bits))
                .unwrap_or_else(|| {
                    MacAddressPoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 24)
                })?,
        ),
        ranges: Some(hw_mac_address_ranges),
    });

    let bmc_mock_certs_dir = app_config.bmc_mock_certs_dir.clone();

    let app_context = Arc::new(MachineATronContext {
        app_config,
        forge_client_config,
        bmc_mock_certs_dir,
        bmc_registry,
        api_throttler,
        desired_firmware_versions: std::sync::RwLock::new(desired_firmware_versions),
        forge_api_client,
        dhcp_client,
        mac_address_pool: Mutex::new(mac_address_pool).into(),
    });

    // Periodically re-fetch the desired firmware versions so target changes in
    // the API reach live machines without a restart (issue #4688). Runs on the
    // same cadence as the per-machine API refresh; each machine re-derives its
    // own targets on its next tick. An empty response is treated as "nothing
    // configured" rather than "clear all targets", so the existing
    // preingestion flow without desired versions is unaffected. Fetch errors
    // keep the last known targets.
    {
        let app_context = app_context.clone();
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(app_context.app_config.api_refresh_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await; // the startup fetch above already populated the context
            loop {
                interval.tick().await;
                match app_context.forge_api_client.get_desired_firmware_versions().await {
                    Ok(response) => {
                        if response.entries.is_empty() {
                            continue;
                        }
                        let mut current =
                            app_context.desired_firmware_versions.write().unwrap();
                        if *current != response.entries {
                            tracing::info!(
                                desired_firmware_versions = ?response.entries,
                                "Desired firmware versions changed; live machines pick up the new targets on their next refresh tick",
                            );
                            *current = response.entries;
                        }
                    }
                    Err(error) => tracing::warn!(
                        %error,
                        "Failed to refresh desired firmware versions; keeping the last known targets",
                    ),
                }
            }
        });
    }

    let info = app_context.forge_api_client.version(false).await?;
    tracing::info!(
        build_version = %info.build_version,
        "machine-a-tron version",
    );

    let mut mat = MachineATron::new(app_context.clone());

    // Machines are created paused here. While paused, their actors do not advance the FSM, so
    // BMC DHCP and shared-router registration cannot run before the combined BMC mock listener is
    // started below.
    let simulators = mat.make_devices(true).await?;

    // Persist them once in case of unclean shutdown
    app_context.app_config.write_persisted_devices(
        simulators
            .devices()
            .iter()
            .map(SimulatorLifecycle::persisted)
            .collect::<Vec<_>>()
            .as_slice(),
    )?;

    // Launch the control UI after the machines are created so it can report their handles. In
    // combined-BMC mode it shares the combined BMC listener. In per-IP mode it listens on the
    // loopback address at the same port, independently of the per-machine BMC listeners.
    let control_state = ControlState::new(
        simulators.clone(),
        DeviceStatusConfig::new(bmc_mock_port),
        inventory_id.into(),
    );
    // Hosted mode mounts the shared UFM mock router on machine-a-tron's control server. Its
    // ControlState can be injected as an in-process inventory provider; the standalone binary
    // initializes the same mock without this provider and relies on configured HTTP sources.
    let hosted_ufm = HostedUfmMock::start(ufm_config, &control_state)?;
    let ufm_router = hosted_ufm.as_ref().map(HostedUfmMock::router);
    let certs_dir = app_context
        .bmc_mock_certs_dir
        .as_ref()
        .cloned()
        .or_else(|| {
            PathBuf::from(forge_root_ca_path.clone())
                .parent()
                .map(Path::to_path_buf)
        });
    let mut server_handle: CombinedServer = {
        let server_config = bmc_mock::tls::server_config(certs_dir.clone())?;
        let bmc_router = bmc_mock::combined_router(app_context.bmc_registry.clone());
        let bmc_router = match ufm_router.clone() {
            Some(ufm_router) => ufm_router.merge(bmc_router),
            None => bmc_router,
        };
        let router = append_control_routes(Some(bmc_router), control_state.clone());
        bmc_mock::CombinedServer::run_router(
            "bmc-mock",
            router,
            Some(ListenerOrAddress::Address(
                format!("0.0.0.0:{bmc_mock_port}").parse().unwrap(),
            )),
            server_config,
        )
    };

    let (stop_tx, stop_rx) = mpsc::channel(1);
    // Create a signal stream for SIGTERM and SIGINT.
    let mut sigterm =
        signal(SignalKind::terminate()).expect("Failed to create SIGTERM signal stream");
    let mut sigint =
        signal(SignalKind::interrupt()).expect("Failed to create SIGINT signal stream");
    tokio::spawn(async move {
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
        stop_tx.send(()).await.ok();
    });

    let mat_result = mat.run(simulators, stop_rx).await;

    if let Some(hosted_ufm) = hosted_ufm {
        hosted_ufm.shutdown().await?;
    }

    server_handle.stop().await?;
    if let Some(dhcp_service) = dhcp_service {
        dhcp_service.shutdown().await?;
    }
    mat_result?;
    Ok(())
}
