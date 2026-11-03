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

use bmc_mock::mac_address_pool::PoolConfig as MacAddressPoolConfig;
use futures::future::try_join_all;
use tokio::sync::mpsc;

use crate::PersistedDevice;
use crate::config::MachineATronContext;
use crate::device_simulator::{
    DeviceSimulator, MachineSimulator, PowerShelfSimulator, SimulatorLifecycle, SwitchSimulator,
};
use crate::expected_inventory::{
    ExpectedInventorySummary, ServedRecords, register_served, served_records, with_retry,
};
use crate::host_machine::HostMachine;
use crate::power_shelf_simulator::PowerShelfActor;
use crate::simulator_registry::SimulatorRegistry;
use crate::status::DeviceKind;
use crate::switch_simulator::SwitchActor;

pub struct MachineATron {
    app_context: Arc<MachineATronContext>,
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
        let (simulators, summary, _) = self.make_devices_with_records(paused).await?;
        Ok((simulators, summary))
    }

    /// `make_devices`, also returning the records built from the devices.
    /// Registration sends exactly these records, so they are what the
    /// control route serves whether or not registration is enabled.
    pub async fn make_devices_with_records(
        &self,
        paused: bool,
    ) -> eyre::Result<(SimulatorRegistry, ExpectedInventorySummary, ServedRecords)> {
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

        let simulators = SimulatorRegistry::builder()
            .devices(devices)
            .racks(resolved_configs.racks.clone())
            .build()?;
        let served = served_records(
            &simulators,
            &resolved_configs.machines,
            &resolved_configs.racks,
        )?;

        let summary = if self.app_context.app_config.register_expected_machines {
            let api_client = self.app_context.api_client();
            register_served(
                &resolved_configs.racks,
                &served,
                || {
                    with_retry("expected rack group lookup", || {
                        api_client.declared_rack_groups()
                    })
                },
                |record| {
                    let api_client = api_client.clone();
                    async move { api_client.add_expected_record(record).await }
                },
            )
            .await?
        } else {
            tracing::info!(
                device_count = simulators.devices().len(),
                "register_expected_machines=false; skipping auto-registration of mock host(s)",
            );
            ExpectedInventorySummary::default()
        };

        Ok((simulators, summary, served))
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
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::Mutex;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use bmc_mock::DpuSettings;
    use bmc_mock::mac_address_pool::{
        Config as MacAddressConfig, MacAddressPool, RangesConfig as MacAddressRangesConfig,
    };
    use mac_address::MacAddress;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;
    use crate::expected_inventory::{DpuKey, ExpectedRecordsResponse};
    use crate::machine_state_machine::OsImage;
    use crate::{ControlState, DeviceStatusConfig, PersistedDpuMachine, append_control_routes};

    const INVENTORY_ID: &str = "mat-06:00:00:00:00:00";

    /// Two hosts with one DPU each, registration disabled so nothing calls
    /// the API, and devices restored from `persist_dir` when it holds a
    /// snapshot.
    fn records_context(persist_dir: Option<&Path>) -> Arc<MachineATronContext> {
        let mut app_context = MachineATronContext::for_test();
        let context = Arc::get_mut(&mut app_context).expect("a new test context is not shared");
        context.app_config.register_expected_machines = false;
        context.app_config.persist_dir = persist_dir.map(Path::to_path_buf);
        let machine = Arc::make_mut(context.app_config.machines.get_mut("config").unwrap());
        machine.host_count = 2;
        machine.dpu_per_host_count = 1;
        context.mac_address_pool = Arc::new(Mutex::new(MacAddressPool::new(MacAddressConfig {
            pool: Some(MacAddressPoolConfig::new(MacAddress::new([2, 0, 0, 0, 0, 0]), 24).unwrap()),
            ranges: Some(
                MacAddressRangesConfig::new(MacAddress::new([6, 0, 0, 0, 0, 0]), 32, 8).unwrap(),
            ),
        })));
        app_context
    }

    async fn make_devices_with_records(
        app_context: Arc<MachineATronContext>,
    ) -> (ExpectedInventorySummary, ServedRecords) {
        let (simulators, summary, served) = MachineATron::new(app_context)
            .make_devices_with_records(true)
            .await
            .unwrap();
        for simulator in simulators.devices() {
            simulator.abort();
        }
        (summary, served)
    }

    fn bmc_macs(served: &ServedRecords) -> BTreeSet<String> {
        served
            .devices
            .iter()
            .map(|device| device.bmc_mac.clone())
            .collect()
    }

    #[tokio::test]
    async fn route_serves_when_registration_disabled() {
        let app_context = records_context(None);
        let (simulators, summary, served) = MachineATron::new(app_context.clone())
            .make_devices_with_records(true)
            .await
            .unwrap();
        assert_eq!(summary, ExpectedInventorySummary::default());
        assert_eq!((served.devices.len(), served.dpus.len()), (2, 2));

        let response = served.into_response(INVENTORY_ID.to_string(), &app_context.app_config);
        let router = append_control_routes(
            None,
            ControlState::new(
                simulators.clone(),
                DeviceStatusConfig::new(1266),
                INVENTORY_ID.into(),
            )
            .with_expected_records(Arc::new(response.clone())),
        );
        let reply = router
            .oneshot(
                Request::builder()
                    .uri("/expected-inventory/records")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        for simulator in simulators.devices() {
            simulator.abort();
        }

        assert_eq!(reply.status(), StatusCode::OK);
        let body = to_bytes(reply.into_body(), usize::MAX).await.unwrap();
        let served: ExpectedRecordsResponse = serde_json::from_slice(&body).unwrap();
        assert!(!served.register_expected_machines);
        assert_eq!(served, response);
    }

    #[tokio::test]
    async fn restored_identity_records_use_persisted_macs() {
        let (_, fresh) = make_devices_with_records(records_context(None)).await;

        let persist_dir = tempfile::tempdir().unwrap();
        let app_context = records_context(Some(persist_dir.path()));
        let hw_type = app_context.app_config.machines["config"].hw_type;
        let persisted = |host: u8| PersistedDevice {
            mat_id: Uuid::new_v4(),
            machine_config_section: "config".to_string(),
            hw_type,
            bmc_mac_address: MacAddress::new([0x0a, 0, 0, 0, host, 1]),
            serial: format!("persisted-{host}"),
            dpus: vec![PersistedDpuMachine {
                mat_id: Uuid::new_v4(),
                hw_type,
                bmc_mac_address: MacAddress::new([0x0a, 0, 0, 0, host, 2]),
                host_mac_address: MacAddress::new([0x0a, 0, 0, 0, host, 3]),
                oob_mac_address: MacAddress::new([0x0a, 0, 0, 0, host, 4]),
                serial: format!("persisted-dpu-{host}"),
                installed_os: OsImage::None,
                dpu_index: 0,
                bmc_state: None,
                settings: DpuSettings::default(),
            }],
            non_dpu_mac_address: None,
            nvos_mac_addresses: Vec::new(),
            switch_serial_number: None,
            observed_machine_id: None,
            installed_os: OsImage::None,
            tpm_ek_certificate: None,
            hw_mac_addr_pool: Some(crate::config::MacAddressPoolConfig {
                base: MacAddress::new([6, 0, 0, 0, host, 0]),
                host_bits: 8,
            }),
            active_host_firmware: None,
            bmc_state: None,
            nvos_admin: None,
        };
        app_context
            .app_config
            .write_persisted_devices(&[persisted(1), persisted(2)])
            .unwrap();

        let (_, restored) = make_devices_with_records(app_context).await;

        let persisted_macs = [1, 2]
            .map(|host| MacAddress::new([0x0a, 0, 0, 0, host, 1]).to_string())
            .into();
        assert_eq!(bmc_macs(&restored), persisted_macs);
        assert!(
            bmc_macs(&fresh).is_disjoint(&persisted_macs),
            "fresh allocation must differ from the persisted identities"
        );
        assert_eq!(
            restored.dpus,
            [1, 2]
                .map(|host| DpuKey {
                    bmc_mac: MacAddress::new([0x0a, 0, 0, 0, host, 2]).to_string(),
                    serial: format!("persisted-dpu-{host}"),
                    parent_bmc_mac: MacAddress::new([0x0a, 0, 0, 0, host, 1]).to_string(),
                })
                .to_vec()
        );
    }
}
