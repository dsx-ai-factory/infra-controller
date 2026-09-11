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
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::process::Command;
use std::sync::Arc;

use axum::Router;
use bmc_mock::mac_address_pool::{
    Config as MacAddressConfig, MacAddressPool, PoolConfig as MacAddressPoolConfig,
    RangesConfig as MacAddressRangesConfig,
};
use bmc_mock::{
    BmcCommand, BmcState, Callbacks, DpuFirmwareVersions, DpuMachineInfo, DpuSettings,
    HardwareType, HostMachineInfo, ListenerOrAddress, MachineInfo, MachineRouterOptions,
    MockPowerState, SetSystemPowerError, SystemPowerControl, VirtualMediaDeviceConfig,
};
use command_line::{MachineRole, StateBackend};
use mac_address::MacAddress;
use tar_router::TarGzOption;
use tokio::sync::{RwLock, mpsc};
use tracing::info;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::prelude::*;

///
/// bmc-mock behaves like a Redfish BMC server
/// Run: 'cargo run'
/// Try it:
///  - start docker-compose things
///  - `cargo make bootstrap-forge-docker`
///  - `grpcurl -d '{"machine_id": {"value": "71363261-a95a-4964-9eb1-8dd98b870746"}}' -insecure
///  127.0.0.1:1079 forge.Forge/CleanupMachineCompleted`
///  where that UUID is a host machine in DB.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut routers_by_ip: HashMap<String, Router> = HashMap::default();

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

    // collection of path to entries map to avoid duplicating entries when multiple machines
    // use the same archive
    let mut tar_router_entries = HashMap::default();

    let args = command_line::parse_args();
    if args.enable_ipmi_simulation && (args.targz.is_some() || args.ip_router.is_some()) {
        return Err(
            "--enable-ipmi-simulation cannot be combined with archive-backed routers".into(),
        );
    }
    let has_generated_options = args.libvirt_domain.is_some()
        || args.hardware_profile.is_some()
        || args.machine_role.is_some()
        || args.state_backend.is_some()
        || args.dpu_count.is_some()
        || args.dpu_index.is_some()
        || args.instance_index != 0;
    if has_generated_options && (args.targz.is_some() || args.ip_router.is_some()) {
        return Err(
            "generated-machine options cannot be combined with archive-backed routers".into(),
        );
    }
    let generated_config = generated_mock_config(&args)?;
    if let Some(ip_routers) = args.ip_router {
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
            .unwrap();
            routers_by_ip.insert(ip_router.ip_address, r);
        }
    }

    let listen_addr = args.port.map(|p| SocketAddr::from(([0, 0, 0, 0], p)));
    info!(cert_path = ?args.cert_path, "Using BMC mock certificate path");
    let (router, generated_state) = if let Some(tar_path) = args.targz {
        info!(archive_path = %tar_path.to_string_lossy(), "Using default BMC mock archive");
        (
            tar_router::tar_router(TarGzOption::Disk(&tar_path), Some(&mut tar_router_entries))
                .unwrap(),
            None,
        )
    } else {
        info!(
            hardware_type = %generated_config.hardware_type,
            role = ?generated_config.machine_role,
            backend = ?generated_config.state_backend,
            "Using generated BMC mock",
        );
        let (router, state) = generated_mock(generated_config);
        (router, Some(state))
    };

    let _ipmi_sim_handle = if args.enable_ipmi_simulation {
        let state = generated_state
            .as_ref()
            .expect("archive-backed routers were rejected above");
        Some(
            bmc_mock::ipmi_sim::start(
                state,
                bmc_mock::ipmi_sim::IpmiSimConfig {
                    stable_id: "standalone-bmc-mock".to_string(),
                    console_prompt: "root@bmc-mock # ".to_string(),
                },
            )
            .await?,
        )
    } else {
        None
    };

    routers_by_ip.insert("".to_owned(), router);

    let server_config = bmc_mock::tls::server_config(args.cert_path)?;
    let mut handle = bmc_mock::CombinedServer::run(
        "bmc-mock",
        Arc::new(RwLock::new(routers_by_ip)),
        listen_addr.map(ListenerOrAddress::Address),
        server_config,
    );
    handle.wait().await?;
    Ok(())
}

fn spawn_qemu_reboot_handler() -> mpsc::UnboundedSender<BmcCommand> {
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Some(command) = command_rx.recv().await else {
                break;
            };
            match command {
                // Assume SetSystemPower is just a reboot
                BmcCommand::SetSystemPower { .. } => {}
                BmcCommand::StateRefreshIndication => continue,
            }
            let reboot_output = match Command::new("virsh")
                .arg("reboot")
                .arg("ManagedHost")
                .output()
            {
                Ok(o) => o,
                Err(err) if matches!(err.kind(), ErrorKind::NotFound) => {
                    tracing::info!("`virsh` not found. Cannot reboot QEMU host.");
                    continue;
                }
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        "Failed to run virsh reboot for managed host",
                    );
                    continue;
                }
            };

            match reboot_output.status.code() {
                Some(0) => {
                    tracing::debug!("Rebooted qemu managed host...");
                }
                Some(exit_code) => {
                    tracing::error!(exit_code, "virsh reboot failed for managed host",);
                    tracing::info!(
                        stdout = %String::from_utf8_lossy(&reboot_output.stdout),
                        "virsh reboot standard output",
                    );
                    tracing::info!(
                        stderr = %String::from_utf8_lossy(&reboot_output.stderr),
                        "virsh reboot standard error",
                    );
                }
                None => {
                    tracing::error!("Reboot command killed by signal");
                }
            }
        }
    });
    command_tx
}

#[derive(Debug)]
struct GeneratedMockConfig {
    machine_role: MachineRole,
    state_backend: StateBackend,
    hardware_type: HardwareType,
    dpu_count: u8,
    dpu_index: usize,
    instance_index: u8,
    dpu_firmware: DpuFirmwareVersions,
    libvirt_config: Option<bmc_mock::libvirt::Config>,
    use_channel_callbacks: bool,
}

fn generated_mock_config(args: &command_line::Args) -> Result<GeneratedMockConfig, String> {
    let explicit_mode = args.machine_role.is_some() || args.state_backend.is_some();
    let libvirt_shorthand_mode =
        args.libvirt_domain.is_some() && args.hardware_profile.is_some() && !explicit_mode;
    let default_generated_mode = args.libvirt_domain.is_none()
        && args.hardware_profile.is_none()
        && !explicit_mode
        && args.dpu_count.is_none()
        && args.dpu_index.is_none()
        && args.instance_index == 0;

    if default_generated_mode {
        return Ok(GeneratedMockConfig {
            machine_role: MachineRole::Host,
            state_backend: StateBackend::Internal,
            hardware_type: HardwareType::WiwynnGB200Nvl,
            dpu_count: 2,
            dpu_index: 0,
            instance_index: 0,
            dpu_firmware: DpuFirmwareVersions::default(),
            libvirt_config: None,
            use_channel_callbacks: true,
        });
    }

    if !explicit_mode && !libvirt_shorthand_mode {
        return Err(
            "explicit generated machines require --hardware-profile, --machine-role, and --state-backend"
                .to_string(),
        );
    }

    let machine_role = if libvirt_shorthand_mode {
        MachineRole::Host
    } else {
        args.machine_role.ok_or_else(|| {
            "--machine-role is required when explicitly configuring a generated machine".to_string()
        })?
    };
    let state_backend = if libvirt_shorthand_mode {
        StateBackend::Libvirt
    } else {
        args.state_backend.ok_or_else(|| {
            "--state-backend is required when explicitly configuring a generated machine"
                .to_string()
        })?
    };
    let hardware_type = args.hardware_profile.ok_or_else(|| {
        "--hardware-profile is required when explicitly configuring a generated machine".to_string()
    })?;

    let dpu_count = match (hardware_type.fixed_number_of_dpu(), args.dpu_count) {
        (Some(fixed), Some(requested)) if fixed != requested => {
            return Err(format!(
                "hardware profile {hardware_type} requires {fixed} DPU(s), not {requested}"
            ));
        }
        (Some(fixed), _) => fixed,
        (None, requested) => requested.unwrap_or(0),
    };
    let dpu_index = args.dpu_index.unwrap_or(0);
    if !args.dpu_firmware.is_empty() && dpu_count == 0 {
        return Err(
            "DPU firmware options require at least one generated DPU; set --dpu-count to a positive value"
                .to_string(),
        );
    }
    match machine_role {
        MachineRole::Host if args.dpu_index.is_some() => {
            return Err("--dpu-index is valid only with --machine-role=dpu".to_string());
        }
        MachineRole::Dpu if dpu_index >= usize::from(dpu_count) => {
            return Err(format!(
                "DPU index {dpu_index} is outside hardware profile {hardware_type}'s {dpu_count} DPU(s)"
            ));
        }
        _ => {}
    }

    let libvirt_config = match (state_backend, &args.libvirt_domain) {
        (StateBackend::Libvirt, Some(domain)) => Some(bmc_mock::libvirt::Config {
            virsh_path: args.virsh_path.clone(),
            uri: args.libvirt_uri.clone(),
            domain: domain.clone(),
            virtual_media_targets: BTreeMap::from([
                ("Cd".to_string(), "sdb".to_string()),
                ("ConfigCd".to_string(), "sdc".to_string()),
            ]),
        }),
        (StateBackend::Libvirt, None) => {
            return Err("--state-backend=libvirt requires --libvirt-domain".to_string());
        }
        (StateBackend::Internal, Some(_)) => {
            return Err("--libvirt-domain requires --state-backend=libvirt".to_string());
        }
        (StateBackend::Internal, None) => None,
    };

    Ok(GeneratedMockConfig {
        machine_role,
        state_backend,
        hardware_type,
        dpu_count,
        dpu_index,
        instance_index: args.instance_index,
        dpu_firmware: args.dpu_firmware.clone().into(),
        libvirt_config,
        use_channel_callbacks: false,
    })
}

fn generated_mock(config: GeneratedMockConfig) -> (Router, BmcState) {
    let machine_info = generated_machine_info(&config);
    let libvirt_callbacks = config
        .libvirt_config
        .map(bmc_mock::libvirt::LibvirtCallbacks::new)
        .map(Arc::new);
    let callbacks: Arc<dyn Callbacks> = if config.use_channel_callbacks {
        let command_channel = spawn_qemu_reboot_handler();
        Arc::new(ChannelCallbacks::new(command_channel))
    } else if let Some(callbacks) = &libvirt_callbacks {
        callbacks.clone()
    } else {
        Arc::new(bmc_mock::simulated::SimulatedCallbacks::new())
    };
    let result = if config.machine_role == MachineRole::Host
        && config.state_backend == StateBackend::Libvirt
    {
        bmc_mock::machine_router(
            &machine_info,
            callbacks,
            String::default(),
            false,
            MachineRouterOptions {
                bmc_reset_duration: None,
                virtual_media_devices: Some(vec![
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
            },
        )
    } else {
        bmc_mock::machine_router(
            &machine_info,
            callbacks,
            String::default(),
            false,
            MachineRouterOptions::default(),
        )
    };
    if let Some(callbacks) = libvirt_callbacks {
        callbacks
            .bind_state(&result.1)
            .expect("libvirt backend must bind to generated BMC state");
    }
    result
}

fn generated_machine_info(config: &GeneratedMockConfig) -> MachineInfo {
    let mut pool = MacAddressPool::new(MacAddressConfig {
        pool: Some(
            MacAddressPoolConfig::new(MacAddress::new([2, 0, 0, config.instance_index, 0, 0]), 16)
                .expect("Must be constructed with these parameters"),
        ),
        ranges: Some(
            MacAddressRangesConfig::new(
                MacAddress::new([6, 0, 0, config.instance_index, 0, 0]),
                16,
                8,
            )
            .expect("Must be constructed with these parameters"),
        ),
    });

    let mac_range = pool
        .allocate_range_config()
        .expect("MAC address pool should be allocated");
    let dpus = (0..config.dpu_count)
        .map(|_| {
            DpuMachineInfo::new(
                config.hardware_type,
                &mut pool,
                DpuSettings {
                    firmware_versions: config.dpu_firmware.clone(),
                    ..DpuSettings::default()
                },
            )
        })
        .collect::<Vec<_>>();
    match config.machine_role {
        MachineRole::Host => MachineInfo::Host(HostMachineInfo::new(
            config.hardware_type,
            dpus,
            &mut pool,
            mac_range,
        )),
        MachineRole::Dpu => MachineInfo::Dpu(dpus[config.dpu_index].clone()),
    }
}

#[derive(Debug)]
struct ChannelCallbacks {
    command_channel: mpsc::UnboundedSender<BmcCommand>,
}

impl ChannelCallbacks {
    fn new(command_channel: mpsc::UnboundedSender<BmcCommand>) -> Self {
        Self { command_channel }
    }
}

impl Callbacks for ChannelCallbacks {
    fn get_power_state(&self) -> MockPowerState {
        MockPowerState::On
    }

    fn send_power_command(
        &self,
        reset_type: SystemPowerControl,
    ) -> Result<(), SetSystemPowerError> {
        self.command_channel
            .send(BmcCommand::SetSystemPower {
                request: reset_type,
                reply: None,
            })
            .map_err(|err| SetSystemPowerError::CommandSendError(err.to_string()))
    }

    fn state_refresh_indication(&self) {
        let _ = self
            .command_channel
            .send(BmcCommand::StateRefreshIndication);
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn config(arguments: &[&str]) -> Result<GeneratedMockConfig, String> {
        let args = command_line::Args::try_parse_from(arguments).unwrap();
        generated_mock_config(&args)
    }

    #[test]
    fn default_generated_mode_uses_channel_callbacks() {
        let config = config(&["bmc-mock"]).unwrap();
        assert_eq!(config.machine_role, MachineRole::Host);
        assert_eq!(config.state_backend, StateBackend::Internal);
        assert!(matches!(config.hardware_type, HardwareType::WiwynnGB200Nvl));
        assert!(config.use_channel_callbacks);
    }

    #[test]
    fn preserves_the_existing_libvirt_shorthand() {
        let config = config(&[
            "bmc-mock",
            "--libvirt-domain",
            "host-01",
            "--hardware-profile",
            "dell_poweredge_r750",
        ])
        .unwrap();
        assert_eq!(config.machine_role, MachineRole::Host);
        assert_eq!(config.state_backend, StateBackend::Libvirt);
        assert!(!config.use_channel_callbacks);
    }

    #[test]
    fn validates_dpu_count_and_index() {
        let cases = [
            [
                "bmc-mock",
                "--machine-role",
                "dpu",
                "--state-backend",
                "internal",
                "--hardware-profile",
                "wiwynn_gb200_nvl",
                "--dpu-count",
                "1",
                "--dpu-index",
                "0",
            ],
            [
                "bmc-mock",
                "--machine-role",
                "dpu",
                "--state-backend",
                "internal",
                "--hardware-profile",
                "wiwynn_gb200_nvl",
                "--dpu-count",
                "2",
                "--dpu-index",
                "2",
            ],
        ];

        for arguments in cases {
            assert!(config(&arguments).is_err(), "arguments {arguments:?}");
        }
    }

    #[test]
    fn rejects_dpu_firmware_overrides_without_generated_dpus() {
        let error = config(&[
            "bmc-mock",
            "--machine-role",
            "host",
            "--state-backend",
            "internal",
            "--hardware-profile",
            "dell_poweredge_r750",
            "--dpu-bmc-firmware",
            "bmc-version",
        ])
        .unwrap_err();

        assert_eq!(
            error,
            "DPU firmware options require at least one generated DPU; set --dpu-count to a positive value",
        );
    }

    #[test]
    fn host_and_dpu_endpoints_share_deterministic_dpu_identity() {
        let host_config = config(&[
            "bmc-mock",
            "--machine-role",
            "host",
            "--state-backend",
            "internal",
            "--hardware-profile",
            "wiwynn_gb200_nvl",
            "--instance-index",
            "3",
        ])
        .unwrap();
        let dpu_config = config(&[
            "bmc-mock",
            "--machine-role",
            "dpu",
            "--state-backend",
            "internal",
            "--hardware-profile",
            "wiwynn_gb200_nvl",
            "--dpu-index",
            "1",
            "--instance-index",
            "3",
        ])
        .unwrap();
        let host = generated_machine_info(&host_config);
        let dpu = generated_machine_info(&dpu_config);
        let MachineInfo::Host(host) = host else {
            panic!("expected host machine info");
        };
        let MachineInfo::Dpu(dpu) = dpu else {
            panic!("expected DPU machine info");
        };

        assert_eq!(host.dpus[1].serial, dpu.serial);
        assert_eq!(host.dpus[1].bmc_mac_address, dpu.bmc_mac_address);
    }

    #[test]
    fn configured_dpu_firmware_reaches_the_generated_host() {
        let config = config(&[
            "bmc-mock",
            "--machine-role",
            "host",
            "--state-backend",
            "internal",
            "--hardware-profile",
            "dell_poweredge_r750",
            "--dpu-count",
            "1",
            "--dpu-bmc-firmware",
            "bmc-version",
            "--dpu-uefi-firmware",
            "uefi-version",
            "--dpu-bsp-firmware",
            "bsp-version",
            "--dpu-cec-firmware",
            "cec-version",
            "--dpu-nic-firmware",
            "nic-version",
        ])
        .unwrap();
        let (_, state) = generated_mock(config);

        for (id, expected) in [
            ("BMC_Firmware", "bmc-version"),
            ("DPU_UEFI", "uefi-version"),
            ("DPU_BSP", "bsp-version"),
            ("Bluefield_FW_ERoT", "cec-version"),
            ("DPU_NIC", "nic-version"),
        ] {
            assert_eq!(
                state
                    .update_service_state
                    .find_firmware_inventory(id)
                    .and_then(|inventory| inventory["Version"].as_str().map(str::to_owned)),
                Some(expected.to_string()),
                "{id}",
            );
        }
    }

    #[test]
    fn configured_dpu_firmware_reaches_generated_bf4_endpoints() {
        for (hardware_profile, machine_role) in [
            ("dell_poweredge_r760_bf4", "host"),
            ("dell_poweredge_r760_bf4", "dpu"),
            ("nvidia_dgx_vr", "host"),
            ("nvidia_dgx_vr", "dpu"),
        ] {
            let config = config(&[
                "bmc-mock",
                "--machine-role",
                machine_role,
                "--state-backend",
                "internal",
                "--hardware-profile",
                hardware_profile,
                "--dpu-bmc-firmware",
                "bmc-version",
                "--dpu-uefi-firmware",
                "uefi-version",
                "--dpu-bsp-firmware",
                "bsp-version",
                "--dpu-cec-firmware",
                "cec-version",
                "--dpu-nic-firmware",
                "nic-version",
            ])
            .unwrap();
            let (_, state) = generated_mock(config);

            for (id, expected) in [
                ("BlueField_FW_BMC_0", "bmc-version"),
                ("BlueField_FW_CPU_0", "uefi-version"),
                ("DPU_BSP", "bsp-version"),
                ("BlueField_FW_ERoT_BMC_0", "cec-version"),
                ("BlueField_FW_NIC_0", "nic-version"),
            ] {
                assert_eq!(
                    state
                        .update_service_state
                        .find_firmware_inventory(id)
                        .and_then(|inventory| inventory["Version"].as_str().map(str::to_owned)),
                    Some(expected.to_string()),
                    "{hardware_profile} {machine_role} {id}",
                );
            }
        }
    }
}
