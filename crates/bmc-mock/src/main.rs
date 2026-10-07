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

mod command_line;
mod tar_router;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use bmc_mock::mac_address_pool::{
    Config as MacAddressConfig, MacAddressPool, PoolConfig as MacAddressPoolConfig,
    RangesConfig as MacAddressRangesConfig,
};
use bmc_mock::{
    DpuMachineInfo, DpuSettings, HostMachineInfo, ListenerOrAddress, MachineInfo,
    MachineRouterOptions, VirtualMediaDeviceConfig, redfish_error_envelope,
};
use command_line::{
    AppType, BmcBehaviorArgs, LibvirtArgs, ListenerArgs, MachineArgs, MachineRole, StateBackend,
    TarGzArgs,
};
use eyre::Context;
use mac_address::MacAddress;
use tar_router::TarGzOption;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::info;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = EnvFilter::from_default_env()
        .add_directive(LevelFilter::DEBUG.into())
        .add_directive("tower=warn".parse().unwrap())
        .add_directive("rustls=warn".parse().unwrap())
        .add_directive("hyper=warn".parse().unwrap())
        .add_directive("h2=warn".parse().unwrap());

    tracing_subscriber::registry()
        .with(Layer::default().compact())
        .with(env_filter)
        .init();

    match command_line::parse_args()? {
        AppType::TarGzRouter { targz, listener } => start_tar_gz_app(targz, listener).await,
        AppType::LibvirtApp {
            listener,
            machine,
            bmc_behaviour,
            libvirt,
        } => start_libvirt_app(listener, machine, bmc_behaviour, libvirt).await,
        AppType::JustMockApp {
            listener,
            machine,
            bmc_behaviour,
        } => start_mock_app(listener, machine, bmc_behaviour).await,
    }
}

async fn start_tar_gz_app(
    args: TarGzArgs,
    listener: ListenerArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // collection of path to entries map to avoid duplicating entries when multiple machines
    // use the same archive
    let mut tar_router_entries = HashMap::default();
    let mut routers_by_ip: HashMap<String, Router> = HashMap::default();
    if let Some(ip_routers) = args.ip_router.as_ref() {
        for ip_router in ip_routers {
            info!(
                archive_path = %ip_router.targz.to_string_lossy(),
                ip_address = %ip_router.ip_address,
                "Using BMC mock archive",
            );
            let r = tar_router::tar_router(
                TarGzOption::Disk(&ip_router.targz),
                Some(&mut tar_router_entries),
            )
            .wrap_err_with(|| format!("failed to load redfish archive {:?}", ip_router.targz))?;
            routers_by_ip.insert(ip_router.ip_address.clone(), r);
        }
    }
    if let Some(tar_path) = args.targz {
        info!(archive_path = %tar_path.to_string_lossy(), "Using default BMC mock archive");
        let router =
            tar_router::tar_router(TarGzOption::Disk(&tar_path), Some(&mut tar_router_entries))
                .wrap_err_with(|| format!("failed to load redfish archive {tar_path:?}"))?;
        routers_by_ip.insert("".to_owned(), router);
    }
    info!(cert_path = ?listener.cert_path, "Using BMC mock certificate path");
    let mut handle = bmc_mock::CombinedServer::run(
        "bmc-mock",
        Arc::new(RwLock::new(routers_by_ip)),
        Some(listener.bind().await?),
        bmc_mock::tls::server_config(listener.cert_path)?,
    );
    handle.wait().await?;
    Ok(())
}

async fn start_libvirt_app(
    listener: ListenerArgs,
    machine: MachineArgs,
    bmc_behaviour: BmcBehaviorArgs,
    libvirt: LibvirtArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut backend_tasks = JoinSet::new();
    info!(
        hardware_type = ?machine.hardware_profile,
        role = ?machine.machine_role,
        backend = ?StateBackend::Libvirt,
        "Using generated BMC mock",
    );
    let stop = CancellationToken::new();
    let guard = stop.clone().drop_guard();
    let (actor, callbacks) = bmc_mock::libvirt::LibvirtActor::new(libvirt.into_config(), guard);
    let (router, state) = bmc_mock::machine_router(
        &machine.machine_info(),
        callbacks.into(),
        String::default(),
        bmc_behaviour.redfish_auth,
        MachineRouterOptions {
            bmc_reset_duration: bmc_behaviour.bmc_reset_duration(),
            virtual_media_devices: machine.libvirt_virtual_media_devices(),
            ..MachineRouterOptions::default()
        },
    );
    actor.run(&state, &mut backend_tasks, stop).await?;
    let _ipmi = if bmc_behaviour.enable_ipmi_simulation {
        Some(bmc_mock::ipmi_sim::start(&state, ipmi_sim_config(), None).await?)
    } else {
        None
    };

    info!(cert_path = ?listener.cert_path, "Using BMC mock certificate path");
    let mut handle = bmc_mock::CombinedServer::run_router(
        "bmc-mock",
        router,
        Some(listener.bind().await?),
        bmc_mock::tls::server_config(listener.cert_path)?,
    );
    tokio::select! {
        result = handle.wait() => result?,
        result = backend_tasks.join_next(), if !backend_tasks.is_empty() => {
            result.expect("backend task set is not empty")?;
            return Err("BMC backend stopped unexpectedly".into());
        }
    }
    backend_tasks.shutdown().await;
    Ok(())
}

async fn start_mock_app(
    listener: ListenerArgs,
    machine: MachineArgs,
    bmc_behaviour: BmcBehaviorArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    info!(
        hardware_type = ?machine.hardware_profile,
        role = ?machine.machine_role,
        backend = ?StateBackend::Internal,
        "Using generated BMC mock",
    );
    let callbacks = Arc::new(bmc_mock::simulated::SimulatedCallbacks::new());
    let (router, state) = {
        bmc_mock::machine_router(
            &machine.machine_info(),
            callbacks,
            String::default(),
            bmc_behaviour.redfish_auth,
            MachineRouterOptions {
                bmc_reset_duration: bmc_behaviour.bmc_reset_duration(),
                ..MachineRouterOptions::default()
            },
        )
    };
    let _ipmi = if bmc_behaviour.enable_ipmi_simulation {
        Some(bmc_mock::ipmi_sim::start(&state, ipmi_sim_config(), None).await?)
    } else {
        None
    };

    info!(cert_path = ?listener.cert_path, "Using BMC mock certificate path");
    let mut handle = bmc_mock::CombinedServer::run_router(
        "bmc-mock",
        router,
        Some(listener.bind().await?),
        bmc_mock::tls::server_config(listener.cert_path)?,
    );
    handle.wait().await?;
    Ok(())
}

impl MachineArgs {
    fn machine_info(&self) -> MachineInfo {
        let instance_index = self.instance_index;
        let mut pool = MacAddressPool::new(MacAddressConfig {
            pool: Some(
                MacAddressPoolConfig::new(MacAddress::new([2, 0, 0, instance_index, 0, 0]), 16)
                    .expect("Must be constructed with these parameters"),
            ),
            ranges: Some(
                MacAddressRangesConfig::new(
                    MacAddress::new([6, 0, 0, instance_index, 0, 0]),
                    16,
                    8,
                )
                .expect("Must be constructed with these parameters"),
            ),
        });
        let hardware_type = self.hardware_profile;
        let dpu_settings = || DpuSettings {
            firmware_versions: self.dpu_firmware.clone().into(),
            ..DpuSettings::default()
        };
        match self.machine_role {
            MachineRole::Host => {
                let dpu_count = self.dpu_count();
                let dpus = (0..dpu_count)
                    .map(|_| DpuMachineInfo::new(hardware_type, &mut pool, dpu_settings()))
                    .collect::<Vec<_>>();
                let mac_range = pool
                    .allocate_range_config()
                    .expect("MAC address pool should be allocated");
                MachineInfo::Host(HostMachineInfo::new(
                    hardware_type,
                    dpus,
                    &mut pool,
                    mac_range,
                ))
            }
            MachineRole::Dpu => {
                let index = self
                    .dpu_index()
                    .expect("Cannot get DPU index for this configuration");
                let dpu = (0..=index)
                    .map(|_| DpuMachineInfo::new(hardware_type, &mut pool, dpu_settings()))
                    .last()
                    .unwrap();
                MachineInfo::Dpu(dpu)
            }
        }
    }

    fn libvirt_virtual_media_devices(&self) -> Option<Vec<VirtualMediaDeviceConfig>> {
        match self.machine_role {
            MachineRole::Host => Some(vec![
                VirtualMediaDeviceConfig {
                    id: Cow::Borrowed("Cd"),
                    name: Cow::Borrowed("Operating System Virtual CD"),
                    media_types: vec![Cow::Borrowed("CD"), Cow::Borrowed("DVD")],
                },
                VirtualMediaDeviceConfig {
                    id: Cow::Borrowed("ConfigCd"),
                    name: Cow::Borrowed("Configuration Virtual CD"),
                    media_types: vec![Cow::Borrowed("CD"), Cow::Borrowed("DVD")],
                },
            ]),
            MachineRole::Dpu => None,
        }
    }
}

impl ListenerArgs {
    fn listener_address(&self) -> SocketAddr {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, self.port.unwrap_or(1266)))
    }

    async fn bind(&self) -> std::io::Result<ListenerOrAddress> {
        // Use explicit dual-stack setup, with an IPv4 fallback when IPv6 is unavailable.
        let listener = metrics_endpoint::bind_tcp_listener(self.listener_address()).await?;
        Ok(ListenerOrAddress::Listener(listener.into_std()?))
    }
}

impl BmcBehaviorArgs {
    fn bmc_reset_duration(&self) -> Option<std::time::Duration> {
        self.bmc_reset_duration
            .filter(|seconds| *seconds > 0)
            .map(std::time::Duration::from_secs)
    }
}

impl LibvirtArgs {
    fn into_config(self) -> bmc_mock::libvirt::Config {
        bmc_mock::libvirt::Config {
            virsh_path: self.virsh_path,
            uri: self.libvirt_uri,
            domain: self
                .libvirt_domain
                .expect("libvirt-domain expected be specified for libvirt backend"),
            virtual_media_targets: BTreeMap::from([
                ("Cd".to_string(), "sdb".to_string()),
                ("ConfigCd".to_string(), "sdc".to_string()),
            ]),
        }
    }
}

fn ipmi_sim_config() -> bmc_mock::ipmi_sim::IpmiSimConfig {
    bmc_mock::ipmi_sim::IpmiSimConfig {
        stable_id: "standalone-bmc-mock".to_string(),
        console_prompt: "root@bmc-mock # ".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    use axum::routing::get;
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn listener_uses_ipv6_wildcard_and_preserves_ports() {
        value_scenarios!(run = |port| ListenerArgs {
            port,
            ..Default::default()
        }.listener_address();
            "configured port" {
                None => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 1266)),
                Some(0) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
            }
        );
    }

    #[tokio::test]
    async fn standalone_listener_serves_https_on_both_families() {
        let ipv6_available = match std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) {
            Ok(_) => true,
            Err(error) => {
                let _listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .expect("IPv4 loopback must be available before skipping IPv6 assertions");
                eprintln!("IPv6 loopback unavailable; checking IPv4 only: {error}");
                false
            }
        };
        let listener = ListenerArgs {
            port: Some(0),
            ..Default::default()
        }
        .bind()
        .await
        .expect("bind standalone listener");
        let mut server = bmc_mock::CombinedServer::run_router(
            "bmc-mock-listener-test",
            Router::new().route("/redfish/v1", get(|| async { "redfish" })),
            Some(listener),
            bmc_mock::tls::server_config(None::<&str>).expect("mock TLS config"),
        );
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("HTTPS client");
        for address in [
            Some(SocketAddr::from((
                Ipv4Addr::LOCALHOST,
                server.address.port(),
            ))),
            ipv6_available.then(|| SocketAddr::from((Ipv6Addr::LOCALHOST, server.address.port()))),
        ]
        .into_iter()
        .flatten()
        {
            let response = client
                .get(format!("https://{address}/redfish/v1"))
                .send()
                .await
                .expect("HTTPS request");
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            assert_eq!(response.text().await.expect("response body"), "redfish");
        }
        server.stop().await.expect("stop mock server");
    }
}
