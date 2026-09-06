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
use std::sync::Arc;

use bmc_mock::HostMachineInfo;
use bmc_mock::mac_address_pool::PoolConfig as MacAddressPoolConfig;
use futures::future::try_join_all;
use model::expected_machine::HostDpuPolicy;
use rpc::forge::{ExpectedInterface, NetworkSegmentType};
use tokio::sync::mpsc;

use crate::PersistedDevice;
use crate::config::MachineATronContext;
use crate::device_simulator::{
    DeviceSimulator, MachineSimulator, PowerShelfSimulator, SimulatorLifecycle, SwitchSimulator,
};
use crate::host_machine::HostMachine;
use crate::power_shelf_simulator::PowerShelfActor;
use crate::simulator_registry::SimulatorRegistry;
use crate::status::DeviceKind;
use crate::switch_simulator::SwitchActor;

pub struct MachineATron {
    app_context: Arc<MachineATronContext>,
}

fn expected_interfaces(
    host_info: &HostMachineInfo,
    dpu_policy: Option<HostDpuPolicy>,
) -> Vec<ExpectedInterface> {
    let mac_addresses = match dpu_policy {
        Some(HostDpuPolicy::Nic) => host_info
            .dpus
            .iter()
            .map(|dpu| dpu.host_mac_address)
            .collect::<Vec<_>>(),
        Some(HostDpuPolicy::Ignore) => host_info.non_dpu_mac_address.into_iter().collect(),
        _ => Vec::new(),
    };

    mac_addresses
        .into_iter()
        .enumerate()
        .map(|(index, mac_address)| ExpectedInterface {
            mac_address: mac_address.to_string(),
            nic_type: None,
            fixed_ip: None,
            fixed_mask: None,
            fixed_gateway: None,
            primary: Some(index == 0),
            network_segment_type: Some(NetworkSegmentType::HostInband as i32),
            ..Default::default()
        })
        .collect()
}

impl MachineATron {
    pub fn new(app_context: Arc<MachineATronContext>) -> Self {
        Self { app_context }
    }

    pub async fn make_devices(&self, paused: bool) -> eyre::Result<SimulatorRegistry> {
        let resolved_configs = self.app_context.app_config.resolved_device_configs()?;

        for (machine_group, machine) in &resolved_configs.machines {
            if machine.missing_host_inband_relay_for_direct_host_dhcp() {
                tracing::warn!(
                    machine_group,
                    dpu_per_host_count = machine.dpu_per_host_count,
                    dpus_in_nic_mode = machine.dpus_in_nic_mode,
                    underlay_dhcp_relay_address = %machine.underlay_dhcp_relay_address,
                    "host_inband_dhcp_relay_address is not configured for a zero-DPU or NIC-mode host; direct host DHCP will fall back to underlay_dhcp_relay_address"
                );
            }
        }

        let mut persisted_devices = self
            .app_context
            .app_config
            .read_persisted_devices()
            .inspect_err(|e| {
                tracing::info!(error=?e, "could not read persisted machines, may be the first run")
            })
            .unwrap_or_default();

        // If we've persisted the machine info on a previous run, use that.
        // Reserve all persisted MACs before allocating anything new, so recovery
        // is independent of config iteration order.
        let devices = {
            let mut mac_address_pool = self.app_context.mac_address_pool.lock().unwrap();

            if let Some(persisted_devices) = persisted_devices.as_ref() {
                for persisted in persisted_devices.values().flatten() {
                    let hw_mac_address_ranges = persisted
                        .hw_mac_addr_pool
                        .as_ref()
                        .map(|pool| MacAddressPoolConfig::new(pool.base, pool.host_bits))
                        .transpose()?;
                    if let Some(hw_mac_address_ranges) = hw_mac_address_ranges {
                        mac_address_pool.reserve_range_config(hw_mac_address_ranges)?;
                    }
                    persisted
                        .mac_addresses()
                        .filter(|addr| {
                            !hw_mac_address_ranges.is_some_and(|range| range.contains(*addr))
                        })
                        .map(|addr| mac_address_pool.reserve(addr))
                        .collect::<Result<Vec<_>, _>>()?;
                }
            }

            resolved_configs
                .machines
                .iter()
                .flat_map(|(config_name, config)| {
                    if let Some(persisted_devices) = persisted_devices
                        .as_mut()
                        .and_then(|m| m.remove(config_name.as_str()))
                    {
                        tracing::info!(
                            config_name = %config_name,
                            "Recovering persisted machines",
                        );
                        persisted_devices
                            .into_iter()
                            .map(|persisted| -> eyre::Result<DeviceSimulator> {
                                let hw_mac_address_ranges = persisted
                                    .hw_mac_addr_pool
                                    .as_ref()
                                    .map(|pool| {
                                        MacAddressPoolConfig::new(pool.base, pool.host_bits)
                                    })
                                    .unwrap_or_else(|| mac_address_pool.allocate_range_config())?;
                                let kind = DeviceKind::from(persisted.hw_type);
                                Ok(match kind {
                                    DeviceKind::Machine => {
                                        DeviceSimulator::Machine(MachineSimulator::new(
                                            HostMachine::from_persisted(
                                                persisted,
                                                config_name.clone(),
                                                self.app_context.clone(),
                                                config.clone(),
                                                hw_mac_address_ranges,
                                            )
                                            .start(paused),
                                        ))
                                    }
                                    DeviceKind::Switch => {
                                        DeviceSimulator::Switch(SwitchSimulator::new(
                                            SwitchActor::from_persisted(
                                                persisted,
                                                config_name.clone(),
                                                self.app_context.clone(),
                                                config.clone(),
                                                hw_mac_address_ranges,
                                            )
                                            .start(paused),
                                        ))
                                    }
                                    DeviceKind::PowerShelf => {
                                        DeviceSimulator::PowerShelf(PowerShelfSimulator::new(
                                            PowerShelfActor::from_persisted(
                                                persisted,
                                                config_name.clone(),
                                                self.app_context.clone(),
                                                config.clone(),
                                                hw_mac_address_ranges,
                                            )
                                            .start(paused),
                                        ))
                                    }
                                    DeviceKind::Dpu => {
                                        unreachable!(
                                            "a configured top-level device cannot be a DPU"
                                        )
                                    }
                                })
                            })
                            .collect::<Vec<_>>()
                    } else {
                        tracing::info!(
                            config_name = %config_name,
                            "Constructing machines",
                        );
                        (0..config.host_count)
                            .map(|_| {
                                let mac_range = mac_address_pool.allocate_range_config()?;
                                Ok(match DeviceKind::from(config.hw_type) {
                                    DeviceKind::Machine => {
                                        DeviceSimulator::Machine(MachineSimulator::new(
                                            HostMachine::new(
                                                self.app_context.clone(),
                                                config_name.clone(),
                                                config.clone(),
                                                &mut mac_address_pool,
                                                mac_range,
                                            )
                                            .start(paused),
                                        ))
                                    }
                                    DeviceKind::Switch => {
                                        DeviceSimulator::Switch(SwitchSimulator::new(
                                            SwitchActor::new(
                                                self.app_context.clone(),
                                                config_name.clone(),
                                                config.clone(),
                                                &mut mac_address_pool,
                                                mac_range,
                                            )
                                            .start(paused),
                                        ))
                                    }
                                    DeviceKind::PowerShelf => {
                                        DeviceSimulator::PowerShelf(PowerShelfSimulator::new(
                                            PowerShelfActor::new(
                                                self.app_context.clone(),
                                                config_name.clone(),
                                                config.clone(),
                                                &mut mac_address_pool,
                                                mac_range,
                                            )
                                            .start(paused),
                                        ))
                                    }
                                    DeviceKind::Dpu => {
                                        unreachable!(
                                            "a configured top-level device cannot be a DPU"
                                        )
                                    }
                                })
                            })
                            .collect::<Vec<_>>()
                    }
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        if self.app_context.app_config.register_expected_machines {
            for rack in &resolved_configs.racks {
                self.app_context
                    .api_client()
                    .ensure_expected_rack(rack.rack_id.clone(), rack.rack_profile_id.clone())
                    .await?;
            }
        }

        let simulators = SimulatorRegistry::builder()
            .devices(devices)
            .racks(resolved_configs.racks)
            .build()?;

        if self.app_context.app_config.register_expected_machines {
            for device in simulators.devices() {
                let machine = device.handle();
                let host_info = machine.host_info();
                let machine_config = resolved_configs
                    .machines
                    .get(machine.machine_config_section())
                    .expect("machine was constructed from a configured machine group");
                let rack_id = machine_config.rack_id.clone();
                let result = match device {
                    DeviceSimulator::PowerShelf(_) => {
                        self.app_context
                            .api_client()
                            .add_expected_power_shelf(
                                host_info.bmc_mac_address.to_string(),
                                host_info.serial.clone(),
                                rack_id,
                            )
                            .await
                    }
                    DeviceSimulator::Switch(_) => {
                        self.app_context
                            .api_client()
                            .add_expected_switch(
                                host_info.bmc_mac_address.to_string(),
                                host_info
                                    .switch_serial_number
                                    .clone()
                                    .unwrap_or_else(|| host_info.serial.clone()),
                                host_info
                                    .nvos_mac_addresses
                                    .iter()
                                    .map(|mac| mac.to_string())
                                    .collect(),
                                rack_id,
                            )
                            .await
                    }
                    DeviceSimulator::Machine(_) => {
                        // Derive the expected `dpu_policy` from the machine's
                        // MachineConfig: zero-DPU hosts declare `Ignore`, hosts
                        // running their DPUs as NICs declare `Nic`, and
                        // everything else defers to the default (`Manage`).
                        // Site-explorer's ingestion gate requires this explicit
                        // declaration for any host without DPU PCIe devices.
                        let dpu_policy = if machine_config.dpu_per_host_count == 0 {
                            Some(HostDpuPolicy::Ignore)
                        } else if machine_config.dpus_in_nic_mode {
                            Some(HostDpuPolicy::Nic)
                        } else {
                            None
                        };
                        let interfaces = expected_interfaces(host_info, dpu_policy);
                        self.app_context
                            .api_client()
                            .add_expected_machine(
                                host_info.bmc_mac_address.to_string(),
                                host_info.serial.clone(),
                                rack_id,
                                dpu_policy,
                                machine_config.dpf_enabled,
                                interfaces,
                            )
                            .await
                    }
                };

                result
                    .inspect_err(|e| {
                        tracing::warn!(
                            error=?e,
                            hardware_type = %host_info.hw_type,
                            "error adding expected inventory record, likely already ingested"
                        );
                    })
                    .ok();
            }
        } else {
            tracing::info!(
                device_count = simulators.devices().len(),
                "register_expected_machines=false; skipping auto-registration of mock host(s)",
            );
        }

        Ok(simulators)
    }

    pub async fn run(
        &mut self,
        simulators: SimulatorRegistry,
        mut stop_rx: mpsc::Receiver<()>,
    ) -> eyre::Result<()> {
        if let Some(host_str) = self
            .app_context
            .app_config
            .configure_carbide_bmc_proxy_host
            .as_ref()
        {
            let host_port_str =
                format!("{}:{}", host_str, self.app_context.app_config.bmc_mock_port);
            tracing::info!(
                bmc_proxy_address = %host_port_str,
                "Configuring carbide API to use as bmc_proxy",
            );
            _ = self
                .app_context
                .api_client()
                .configure_bmc_proxy_host(host_port_str)
                .await
                .inspect_err(
                    |e| tracing::warn!(error = ?e, "Could not configure carbide bmc_proxy"),
                )
        }

        for simulator in simulators.devices() {
            simulator.resume()?;
        }

        tracing::info!("Machine construction complete");

        let _ = stop_rx.recv().await;
        tracing::info!("quit");
        let cleanup_on_quit = self.app_context.app_config.cleanup_on_quit;
        let persisted_devices =
            try_join_all(simulators.devices().iter().cloned().map(|simulator| {
                let api_client = self.app_context.api_client();
                let persisted = simulator.persisted();
                async move {
                    simulator.shutdown().await?;
                    if cleanup_on_quit {
                        simulator.delete_from_api(api_client).await?;
                    }
                    Ok::<PersistedDevice, eyre::Report>(persisted)
                }
            }))
            .await?;

        // Persist the current state of the machines before quitting
        self.app_context
            .app_config
            .write_persisted_devices(&persisted_devices)?;

        if self
            .app_context
            .app_config
            .configure_carbide_bmc_proxy_host
            .is_some()
        {
            tracing::info!("Removing bmc_proxy configuration from carbide API");
            _ = self
                .app_context
                .api_client()
                .configure_bmc_proxy_host("".to_string())
                .await
                .inspect_err(
                    |e| tracing::warn!(error = ?e, "Could not configure carbide bmc_proxy"),
                )
        }

        tracing::info!("machine-a-tron finished");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use bmc_mock::{DpuMachineInfo, DpuSettings, HardwareType};
    use carbide_test_support::{Check, check_values};
    use mac_address::MacAddress;

    use super::*;

    fn mac(value: &str) -> MacAddress {
        MacAddress::from_str(value).unwrap()
    }

    fn host_info(dpu_host_macs: &[MacAddress], non_dpu_mac: Option<MacAddress>) -> HostMachineInfo {
        HostMachineInfo {
            hw_type: HardwareType::WiwynnGB200Nvl,
            rack_placement: None,
            bmc_mac_address: mac("02:00:00:00:00:f0"),
            serial: "test-host".to_string(),
            dpus: dpu_host_macs
                .iter()
                .enumerate()
                .map(|(index, host_mac_address)| DpuMachineInfo {
                    hw_type: HardwareType::WiwynnGB200Nvl,
                    bmc_mac_address: mac(&format!("02:00:00:00:10:{index:02x}")),
                    host_mac_address: *host_mac_address,
                    oob_mac_address: mac(&format!("02:00:00:00:20:{index:02x}")),
                    serial: format!("test-dpu-{index}"),
                    settings: DpuSettings::default(),
                })
                .collect(),
            non_dpu_mac_address: non_dpu_mac,
            nvos_mac_addresses: Vec::new(),
            switch_serial_number: None,
            hw_mac_addr_pool: MacAddressPoolConfig::new(mac("0a:00:00:00:00:00"), 24).unwrap(),
            delta_psu_power: None,
            initial_host_firmware: None,
            desired_host_firmware: None,
        }
    }

    fn expected_nic(mac_address: MacAddress, primary: bool) -> ExpectedInterface {
        ExpectedInterface {
            mac_address: mac_address.to_string(),
            nic_type: None,
            fixed_ip: None,
            fixed_mask: None,
            fixed_gateway: None,
            primary: Some(primary),
            network_segment_type: Some(NetworkSegmentType::HostInband as i32),
            ..Default::default()
        }
    }

    #[test]
    fn expected_interface_derivation() {
        let first_dpu_mac = mac("02:00:00:00:00:01");
        let second_dpu_mac = mac("02:00:00:00:00:02");
        let integrated_mac = mac("02:00:00:00:00:03");

        check_values(
            [
                Check {
                    scenario: "NIC-mode host declares every host-facing DPU PF",
                    input: (
                        host_info(&[first_dpu_mac, second_dpu_mac], None),
                        Some(HostDpuPolicy::Nic),
                    ),
                    expect: vec![
                        expected_nic(first_dpu_mac, true),
                        expected_nic(second_dpu_mac, false),
                    ],
                },
                Check {
                    scenario: "zero-DPU host declares its integrated NIC",
                    input: (
                        host_info(&[], Some(integrated_mac)),
                        Some(HostDpuPolicy::Ignore),
                    ),
                    expect: vec![expected_nic(integrated_mac, true)],
                },
                Check {
                    scenario: "managed-DPU host relies on automatic DPU discovery",
                    input: (host_info(&[first_dpu_mac], None), None),
                    expect: Vec::new(),
                },
            ],
            |(host_info, dpu_policy)| expected_interfaces(&host_info, dpu_policy),
        );
    }
}
