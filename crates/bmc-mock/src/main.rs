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
mod listener;
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
    let callbacks = Arc::new(callbacks);
    let (router, state) = bmc_mock::machine_router(
        &machine.machine_info(),
        callbacks.clone(),
        String::default(),
        bmc_behaviour.redfish_auth,
        MachineRouterOptions {
            bmc_reset_duration: bmc_behaviour.bmc_reset_duration(),
            virtual_media_devices: Some(machine.libvirt_virtual_media_devices()),
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
    let result: Result<(), Box<dyn std::error::Error>> = tokio::select! {
        result = handle.wait() => result.map_err(Into::into),
        result = async {
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            tokio::select! {
                result = tokio::signal::ctrl_c() => result,
                _ = terminate.recv() => Ok(()),
            }
        } => result.map_err(Into::into),
        result = backend_tasks.join_next(), if !backend_tasks.is_empty() => {
            match result.expect("backend task set is not empty") {
                Ok(()) => Err("BMC backend stopped unexpectedly".into()),
                Err(error) => Err(error.into()),
            }
        }
    };
    // Stop accepting mutations before taking the final snapshot.
    let server_result = handle.stop().await;
    let persistence_result = callbacks.finish().await;
    backend_tasks.shutdown().await;
    result?;
    server_result?;
    persistence_result?;
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
    let stop = CancellationToken::new();
    let (actor, callbacks) = bmc_mock::simulated::SimulatedActor::new(stop.clone().drop_guard());
    let callbacks = Arc::new(callbacks);
    let mut backend_tasks = JoinSet::new();
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
    actor.run(&state, &mut backend_tasks, stop);
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
    let result: Result<(), Box<dyn std::error::Error>> = tokio::select! {
        result = handle.wait() => result.map_err(Into::into),
        result = backend_tasks.join_next() => {
            match result.expect("simulated backend task set is not empty") {
                Ok(()) => Err("simulated BMC backend stopped unexpectedly".into()),
                Err(error) => Err(error.into()),
            }
        }
    };
    let server_result = handle.stop().await;
    backend_tasks.shutdown().await;
    result?;
    server_result?;
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

    fn libvirt_virtual_media_devices(&self) -> Vec<VirtualMediaDeviceConfig> {
        let config_media = VirtualMediaDeviceConfig {
            id: Cow::Borrowed("ConfigCd"),
            name: Cow::Borrowed("Configuration Virtual CD"),
            media_types: vec![Cow::Borrowed("CD"), Cow::Borrowed("DVD")],
        };
        match self.machine_role {
            MachineRole::Host => vec![
                VirtualMediaDeviceConfig {
                    id: Cow::Borrowed("Cd"),
                    name: Cow::Borrowed("Operating System Virtual CD"),
                    media_types: vec![Cow::Borrowed("CD"), Cow::Borrowed("DVD")],
                },
                config_media,
            ],
            MachineRole::Dpu => vec![config_media],
        }
    }
}

impl ListenerArgs {
    fn listener_address(&self) -> SocketAddr {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, self.port.unwrap_or(1266)))
    }

    async fn bind(&self) -> std::io::Result<ListenerOrAddress> {
        // Use explicit dual-stack setup, with an IPv4 fallback when IPv6 is unavailable.
        let listener = listener::bind_tcp_listener(self.listener_address()).await?;
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
            state_file: self.state_file,
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

    use super::*;

    struct MediaCallbacks;

    impl bmc_mock::Callbacks for MediaCallbacks {
        async fn computer_system_reset(
            &self,
            _reset_type: bmc_mock::ResourceResetType,
        ) -> Result<(), bmc_mock::ActionError> {
            Ok(())
        }

        fn state_refresh_indication(&self) {}
    }

    async fn redfish_request(
        router: &Router,
        method: axum::http::Method,
        path: &str,
        payload: Option<serde_json::Value>,
    ) -> (axum::http::StatusCode, Option<serde_json::Value>) {
        use axum::body::{Body, to_bytes};
        use tower::ServiceExt;

        let request = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("Content-Type", "application/json")
            .body(
                payload
                    .map(|value| Body::from(value.to_string()))
                    .unwrap_or_else(Body::empty),
            )
            .unwrap();
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            (!body.is_empty()).then(|| serde_json::from_slice(&body).unwrap()),
        )
    }

    #[tokio::test]
    async fn libvirt_media_routes_follow_machine_role() {
        use axum::http::{Method, StatusCode};
        use bmc_mock::HardwareType;

        for (role, expected) in [
            (MachineRole::Dpu, vec!["ConfigCd"]),
            (MachineRole::Host, vec!["Cd", "ConfigCd"]),
        ] {
            let machine = MachineArgs {
                machine_role: role,
                hardware_profile: HardwareType::GenericAmi,
                dpu_count: None,
                dpu_index: None,
                instance_index: 0,
                dpu_firmware: Default::default(),
            };
            let (router, _state) = bmc_mock::machine_router(
                &machine.machine_info(),
                Arc::new(MediaCallbacks),
                String::default(),
                false,
                MachineRouterOptions {
                    virtual_media_devices: Some(machine.libvirt_virtual_media_devices()),
                    ..Default::default()
                },
            );
            let (status, systems) =
                redfish_request(&router, Method::GET, "/redfish/v1/Systems", None).await;
            assert_eq!(status, StatusCode::OK);
            let systems = systems.unwrap();
            let mut media_systems = 0;
            for member in systems["Members"].as_array().unwrap() {
                let system_path = member["@odata.id"].as_str().unwrap();
                let (status, system) =
                    redfish_request(&router, Method::GET, system_path, None).await;
                assert_eq!(status, StatusCode::OK);
                let system = system.unwrap();
                let Some(collection_path) = system["VirtualMedia"]["@odata.id"].as_str() else {
                    continue;
                };
                media_systems += 1;
                let (status, collection) =
                    redfish_request(&router, Method::GET, collection_path, None).await;
                assert_eq!(status, StatusCode::OK);
                let collection = collection.unwrap();
                let members = collection["Members"].as_array().unwrap();
                assert_eq!(members.len(), expected.len(), "{role:?}");
                for (member, id) in members.iter().zip(&expected) {
                    let path = member["@odata.id"].as_str().unwrap();
                    let (status, drive) = redfish_request(&router, Method::GET, path, None).await;
                    assert_eq!(status, StatusCode::OK);
                    let drive = drive.unwrap();
                    assert_eq!(drive["Id"], *id);
                    assert_eq!(drive["Inserted"], false);
                    if role == MachineRole::Dpu {
                        let image = "https://images.example.com/config.iso";
                        let insert = drive["Actions"]["#VirtualMedia.InsertMedia"]["target"]
                            .as_str()
                            .unwrap();
                        let (status, _) = redfish_request(
                            &router,
                            Method::POST,
                            insert,
                            Some(serde_json::json!({"Image": image})),
                        )
                        .await;
                        assert_eq!(status, StatusCode::NO_CONTENT);
                        let (_, drive) = redfish_request(&router, Method::GET, path, None).await;
                        let drive = drive.unwrap();
                        assert_eq!(drive["Image"], image);
                        assert_eq!(drive["Inserted"], true);
                        let eject = drive["Actions"]["#VirtualMedia.EjectMedia"]["target"]
                            .as_str()
                            .unwrap();
                        let (status, _) = redfish_request(
                            &router,
                            Method::POST,
                            eject,
                            Some(serde_json::json!({})),
                        )
                        .await;
                        assert_eq!(status, StatusCode::NO_CONTENT);
                        let (_, drive) = redfish_request(&router, Method::GET, path, None).await;
                        assert_eq!(drive.unwrap()["Inserted"], false);
                    }
                }
                if role == MachineRole::Dpu {
                    let (status, _) = redfish_request(
                        &router,
                        Method::GET,
                        &format!("{collection_path}/Cd"),
                        None,
                    )
                    .await;
                    assert_eq!(status, StatusCode::NOT_FOUND);
                }
            }
            assert_eq!(media_systems, 1, "{role:?}");
        }
    }

    #[test]
    fn listener_uses_ipv6_wildcard_and_preserves_ports() {
        for (scenario, port, expected) in [
            (
                "configured port",
                None,
                SocketAddr::from((Ipv6Addr::UNSPECIFIED, 1266)),
            ),
            (
                "configured port",
                Some(0),
                SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
            ),
        ] {
            let actual = ListenerArgs {
                port,
                ..Default::default()
            }
            .listener_address();
            assert_eq!(actual, expected, "{scenario}");
        }
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
