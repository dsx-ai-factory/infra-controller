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
use futures::future::join_all;
use model::expected_machine::HostDpuPolicy;
use rpc::forge::{ExpectedInterface, NetworkSegmentType};
use tokio::sync::mpsc;

use crate::PersistedDevice;
use crate::api_client::ExpectedRecord;
use crate::config::MachineATronContext;
use crate::device_simulator::{
    DeviceSimulator, MachineSimulator, PowerShelfSimulator, SimulatorLifecycle, SwitchSimulator,
};
use crate::expected_inventory::{CONCURRENCY, ExpectedInventorySummary, register_all};
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

    /// Builds the simulators and, when `register_expected_machines` is set,
    /// registers their expected inventory records, failing if any record
    /// cannot be registered. The summary counts are all zero when
    /// registration is disabled.
    pub async fn make_devices(
        &self,
        paused: bool,
    ) -> eyre::Result<(SimulatorRegistry, ExpectedInventorySummary)> {
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
            let racks = resolved_configs
                .racks
                .iter()
                .map(|rack| ExpectedRecord::Rack {
                    rack_id: rack.rack_id.clone(),
                    rack_profile_id: rack.rack_profile_id.clone(),
                })
                .collect();
            let api_client = self.app_context.api_client();
            let failed = register_all(racks, CONCURRENCY, |record| {
                let api_client = api_client.clone();
                async move { api_client.add_expected_record(record).await }
            })
            .await
            .failed_identifiers;
            if !failed.is_empty() {
                eyre::bail!("failed to register expected {}", failed.join(", "));
            }
        }

        let simulators = SimulatorRegistry::builder()
            .devices(devices)
            .racks(resolved_configs.racks)
            .build()?;

        let summary = if self.app_context.app_config.register_expected_machines {
            let records = simulators
                .devices()
                .iter()
                .map(|device| {
                    let machine = device.handle();
                    let host_info = machine.host_info();
                    let machine_config = resolved_configs
                        .machines
                        .get(machine.machine_config_section())
                        .expect("machine was constructed from a configured machine group");
                    let rack_id = machine_config.rack_id.clone();
                    match device {
                        DeviceSimulator::PowerShelf(_) => ExpectedRecord::PowerShelf {
                            bmc_mac_address: host_info.bmc_mac_address.to_string(),
                            shelf_serial_number: host_info.serial.clone(),
                            rack_id,
                        },
                        DeviceSimulator::Switch(_) => ExpectedRecord::Switch {
                            bmc_mac_address: host_info.bmc_mac_address.to_string(),
                            switch_serial_number: host_info
                                .switch_serial_number
                                .clone()
                                .unwrap_or_else(|| host_info.serial.clone()),
                            nvos_mac_addresses: host_info
                                .nvos_mac_addresses
                                .iter()
                                .map(|mac| mac.to_string())
                                .collect(),
                            rack_id,
                        },
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
                            ExpectedRecord::Machine {
                                bmc_mac_address: host_info.bmc_mac_address.to_string(),
                                chassis_serial_number: host_info.serial.clone(),
                                rack_id,
                                dpu_policy,
                                dpf_enabled: machine_config.dpf_enabled,
                                interfaces: expected_interfaces(host_info, dpu_policy),
                            }
                        }
                    }
                })
                .collect::<Vec<_>>();

            let api_client = self.app_context.api_client();
            let summary = register_all(records, CONCURRENCY, |record| {
                let api_client = api_client.clone();
                async move { api_client.add_expected_record(record).await }
            })
            .await;
            summary.log();
            if !summary.failed_identifiers.is_empty() {
                let failed = &summary.failed_identifiers;
                let mut listed = failed.iter().take(20).cloned().collect::<Vec<_>>();
                if failed.len() > listed.len() {
                    listed.push(format!("and {} more", failed.len() - listed.len()));
                }
                eyre::bail!(
                    "failed to register {} expected device records: {}",
                    failed.len(),
                    listed.join(", ")
                );
            }
            summary
        } else {
            tracing::info!(
                device_count = simulators.devices().len(),
                "register_expected_machines=false; skipping auto-registration of mock host(s)",
            );
            ExpectedInventorySummary::default()
        };

        Ok((simulators, summary))
    }

    /// `shutdown_devices` snapshots every device, optionally deletes its API records,
    /// and stops its actors. Cleanup runs while DPU actors can acknowledge Admin
    /// networking. Every device is stopped before returning an error; a device's
    /// cleanup error takes precedence over its shutdown error, which is also logged.
    /// On success, returns the snapshots in registry order without persisting them.
    pub async fn shutdown_devices(
        &self,
        simulators: &SimulatorRegistry,
        cleanup: bool,
    ) -> eyre::Result<Vec<PersistedDevice>> {
        join_all(simulators.devices().iter().map(|simulator| {
            let api_client = self.app_context.api_client();
            let persisted = simulator.persisted();
            async move {
                let cleanup_result = if cleanup {
                    simulator
                        .delete_from_api(api_client)
                        .await
                        .inspect_err(|error| {
                            tracing::warn!(
                                mat_id = %persisted.mat_id,
                                error = %error,
                                "Failed to delete simulator API records",
                            );
                        })
                } else {
                    Ok(())
                };
                let shutdown_result = simulator.shutdown().await.inspect_err(|error| {
                    tracing::warn!(
                        mat_id = %persisted.mat_id,
                        error = %error,
                        "Failed to shut down simulator",
                    );
                });
                cleanup_result?;
                shutdown_result?;
                Ok(persisted)
            }
        }))
        .await
        .into_iter()
        .collect()
    }

    pub async fn run(
        &mut self,
        simulators: SimulatorRegistry,
        mut stop_rx: mpsc::Receiver<()>,
    ) -> eyre::Result<()> {
        if let Some(bmc_proxy_address) = self.app_context.app_config.bmc_proxy_address() {
            tracing::info!(
                %bmc_proxy_address,
                "Configuring carbide API to use as bmc_proxy",
            );
            _ = self
                .app_context
                .api_client()
                .configure_bmc_proxy_host(bmc_proxy_address)
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
        let persisted_devices = self
            .shutdown_devices(&simulators, self.app_context.app_config.cleanup_on_quit)
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
    use std::time::Duration;

    use bmc_mock::mac_address_pool::{Config as MacAddressConfig, MacAddressPool, RangesConfig};
    use bmc_mock::{DpuMachineInfo, DpuSettings, HardwareType};
    use carbide_test_support::{Check, check_values};
    use mac_address::MacAddress;
    use rpc::forge_tls_client::{ApiConfig, RetryConfig};
    use rpc::protos::forge_api_client::ForgeApiClient;

    use super::*;
    use crate::api_client::ClientApiError;

    #[tokio::test]
    async fn run_stops_all_actors_even_when_cleanup_fails() {
        struct Case {
            scenario: &'static str,
            cleanup_on_quit: bool,
            check: fn(eyre::Result<()>),
        }

        for case in [
            Case {
                scenario: "cleanup failure is returned after stopping every actor",
                cleanup_on_quit: true,
                check: |result| {
                    let error = result.expect_err("the API is unavailable");
                    assert!(
                        matches!(
                            error.downcast_ref::<ClientApiError>(),
                            Some(ClientApiError::InvocationError(status))
                                if status.code() == tonic::Code::Unavailable
                        ),
                        "unexpected cleanup error: {error:?}",
                    );
                },
            },
            Case {
                scenario: "cleanup disabled stops every actor without calling the API",
                cleanup_on_quit: false,
                check: |result| result.expect("shutdown without cleanup succeeds"),
            },
        ] {
            let mut app_context = MachineATronContext::for_test();
            let context = Arc::get_mut(&mut app_context).expect("test owns the context");
            context.app_config.cleanup_on_quit = case.cleanup_on_quit;
            context.app_config.register_expected_machines = false;
            // Use plaintext so a missing test CA cannot mask the connection failure.
            context.app_config.carbide_api_url = "http://127.0.0.1:1".to_string();
            let machine_config = Arc::make_mut(
                context
                    .app_config
                    .machines
                    .get_mut("config")
                    .expect("test machine config"),
            );
            machine_config.host_count = 2;
            machine_config.dpu_per_host_count = 1;
            *context.mac_address_pool.lock().expect("test MAC pool lock") =
                MacAddressPool::new(MacAddressConfig {
                    pool: Some(
                        MacAddressPoolConfig::new(mac("02:00:00:00:00:00"), 24)
                            .expect("test MAC pool"),
                    ),
                    ranges: Some(
                        RangesConfig::new(mac("06:00:00:00:00:00"), 32, 8)
                            .expect("test hardware MAC ranges"),
                    ),
                });
            context.forge_client_config.connect_retries_max = Some(0);
            context.forge_client_config.connect_retries_interval = Some(Duration::from_millis(1));
            context.forge_client_config.request_timeout = Some(Duration::from_secs(1));
            context.forge_api_client = ForgeApiClient::new(
                &ApiConfig::new(
                    &context.app_config.carbide_api_url,
                    &context.forge_client_config,
                )
                .with_retry_config(RetryConfig {
                    retries: 0,
                    interval: Duration::from_millis(1),
                }),
            );

            let mut mat = MachineATron::new(app_context);
            let (simulators, _) = mat.make_devices(true).await.expect("create real actors");
            let handles = simulators.provisionable_handles();
            assert_eq!(handles.len(), 2, "{}", case.scenario);
            for handle in &handles {
                handle.pause().expect("host actor is alive before shutdown");
                assert_eq!(handle.dpus().len(), 1, "{}", case.scenario);
                handle.dpus()[0]
                    .pause()
                    .expect("DPU actor is alive before shutdown");
            }

            let (stop_tx, stop_rx) = mpsc::channel(1);
            stop_tx.send(()).await.expect("queue stop before running");
            let result =
                tokio::time::timeout(Duration::from_secs(30), mat.run(simulators, stop_rx))
                    .await
                    .expect("shutdown must finish within 30 seconds");

            for handle in &handles {
                assert!(
                    handle.pause().is_err(),
                    "{}: host actor still running",
                    case.scenario,
                );
                assert!(
                    handle.dpus()[0].pause().is_err(),
                    "{}: DPU actor still running",
                    case.scenario,
                );
            }
            (case.check)(result);
        }
    }

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
