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

use std::net::IpAddr;
use std::sync::Arc;

use carbide_api_core::test_support::Api;
use carbide_api_core::test_support::fixture_config::FixtureDefault as _;
use carbide_site_explorer::test_support::TestSiteExplorer;
use carbide_uuid::machine::HostMachineId;
use chrono::Utc;
use mac_address::MacAddress;
use model::expected_machine::{ExpectedMachine, ExpectedMachineData};
use model::hardware_info::HardwareInfo;
use model::machine::ManagedHostState;
use model::machine::machine_id::host_id_from_dpu_hardware_info;
use model::site_explorer::EndpointExplorationReport;
use model::test_support::ManagedHostConfig;

use crate::TestHarness;
use crate::db_machine::DbMachineExt as _;
use crate::machine::TestMachine;
use crate::machine_dpu::TestDpuMachine;
use crate::machine_host::TestHostMachine;
use crate::network::segment::TestNetworkSegment;
use crate::rpc::forge::forge_server::Forge;
use crate::rpc::forge::{DhcpDiscovery, HealthReportEntry, InsertMachineHealthReportRequest};

#[derive(Clone)]
pub struct TestManagedHost {
    pub host: TestHostMachine,
    pub dpus: Vec<TestDpuMachine>,
    pub api: Arc<Api>,
}

impl TestManagedHost {
    pub fn dpu(&self, dpu_index: usize) -> &TestDpuMachine {
        self.dpus
            .get(dpu_index)
            .unwrap_or_else(|| panic!("DPU {dpu_index} should exist"))
    }

    pub fn first_dpu(&self) -> &TestDpuMachine {
        self.dpu(0)
    }

    pub async fn insert_empty_host_health_report(&self, source: impl Into<String>) {
        self.api
            .insert_machine_health_report(tonic::Request::new(InsertMachineHealthReportRequest {
                health_report_entry: Some(HealthReportEntry {
                    report: Some(crate::rpc::health::HealthReport {
                        source: source.into(),
                        triggered_by: None,
                        observed_at: None,
                        successes: vec![],
                        alerts: vec![],
                    }),
                    ..Default::default()
                }),
                machine_id: Some(self.host.id.into()),
            }))
            .await
            .expect("empty host health report should be inserted");
    }

    pub async fn advance_state(&self, state: ManagedHostState) {
        let mut txn = self
            .api
            .database_connection
            .begin()
            .await
            .expect("database transaction should start");
        let machine = self.host.db_machine(&mut txn).await;
        machine.advance_state(&mut txn, state).await;
        txn.commit()
            .await
            .expect("database transaction should commit");
    }

    /// Advances this fixture to a stable Ready state after recording its
    /// current desired boot interface as verified.
    ///
    /// This models the invariant established by the machine controller before
    /// it returns a host to Ready. Use [`Self::advance_state`] instead when a
    /// test intentionally needs a Ready host with pending boot configuration.
    pub async fn advance_to_converged_ready(&self) {
        let mut txn = self
            .api
            .database_connection
            .begin()
            .await
            .expect("database transaction should start");
        let machine = self.host.db_machine(&mut txn).await;
        let desired_version = machine
            .config
            .desired_boot_interface
            .as_ref()
            .expect("test host should have a desired boot interface")
            .version;
        assert!(
            db::machine_desired_boot_interface::mark_verified(
                txn.as_mut(),
                &self.host.id,
                desired_version,
                Utc::now(),
            )
            .await
            .expect("boot-interface verification should be recorded"),
            "test host's desired boot interface should still be current"
        );
        machine
            .advance_state(&mut txn, ManagedHostState::Ready)
            .await;
        txn.commit()
            .await
            .expect("database transaction should commit");
    }

    pub async fn report_dpu_network_status(&self) {
        for dpu in &self.dpus {
            dpu.record_network_status().await;
        }
    }
}

#[derive(Clone)]
pub struct TestManagedHostBuildData {
    host_bmc_ip: IpAddr,
    dpu_bmc_ips: Vec<IpAddr>,
}

impl TestManagedHostBuildData {
    pub fn host_bmc_ip(&self) -> IpAddr {
        self.host_bmc_ip
    }

    pub fn dpu_bmc_ip(&self, dpu_index: usize) -> IpAddr {
        *self
            .dpu_bmc_ips
            .get(dpu_index)
            .unwrap_or_else(|| panic!("DPU {dpu_index} BMC IP should exist"))
    }

    pub fn first_dpu_bmc_ip(&self) -> IpAddr {
        self.dpu_bmc_ip(0)
    }
}

pub struct TestManagedHostBuilder<'a> {
    test_harness: &'a TestHarness,
    site_explorer: &'a TestSiteExplorer,
    segment: TestNetworkSegment,
    config: Option<ManagedHostConfig>,
    dpu_primary_segment: Option<TestNetworkSegment>,
    report_dpu_network_status: bool,
}

impl<'a> TestManagedHostBuilder<'a> {
    pub(crate) fn new(
        test_harness: &'a TestHarness,
        site_explorer: &'a TestSiteExplorer,
        segment: TestNetworkSegment,
    ) -> Self {
        Self {
            test_harness,
            site_explorer,
            segment,
            config: None,
            dpu_primary_segment: None,
            report_dpu_network_status: false,
        }
    }

    pub fn with_dpu_network_status_reported(self) -> Self {
        Self {
            report_dpu_network_status: true,
            ..self
        }
    }

    pub fn with_config(self, config: ManagedHostConfig) -> Self {
        Self {
            config: Some(config),
            ..self
        }
    }

    pub fn with_dpu_primary_interfaces(self, segment: TestNetworkSegment) -> Self {
        Self {
            dpu_primary_segment: Some(segment),
            ..self
        }
    }

    pub async fn build(self) -> (TestManagedHost, TestManagedHostBuildData) {
        let config = self.config.unwrap_or_else(ManagedHostConfig::default);
        register_expected_machine(self.test_harness, &config).await;

        if let Some(segment) = self.dpu_primary_segment {
            for dpu in &config.dpus {
                self.test_harness
                    .api()
                    .discover_dhcp(
                        DhcpDiscovery::builder(dpu.oob_mac_address, segment.relay_address)
                            .vendor_string("SomeVendor")
                            .tonic_request(),
                    )
                    .await
                    .expect("DPU primary interface DHCP discovery should succeed");
            }
        }

        let host_bmc_ip = discover_bmc(
            self.test_harness.api(),
            config.bmc_mac_address,
            self.segment,
            "SomeVendor",
        )
        .await;
        let mut dpu_bmc_ips = Vec::new();
        for (dpu_index, dpu) in config.dpus.iter().enumerate() {
            let dpu_index = dpu_index.try_into().expect("DPU index should fit into u8");
            let bmc_ip = discover_bmc(
                self.test_harness.api(),
                dpu.bmc_mac_address,
                self.segment,
                "NVIDIA/BF/BMC",
            )
            .await;
            dpu_bmc_ips.push((dpu_index, bmc_ip));
        }

        let results = config
            .exploration_results(Some(host_bmc_ip), &dpu_bmc_ips)
            .expect("managed host exploration results should be generated");
        let dpu_machine_ids = results.dpu_machine_ids();
        self.site_explorer
            .insert_endpoints(results.into_endpoints());

        // First iteration explores the endpoints. Preingestion then completes
        // outside site-explorer, and the second iteration creates the managed host.
        self.site_explorer
            .run_single_iteration()
            .await
            .expect("first site explorer iteration should succeed");

        let mut txn = self.test_harness.db_txn().await;
        db::explored_endpoints::set_preingestion_complete(host_bmc_ip, &mut txn)
            .await
            .expect("host endpoint preingestion should be marked complete");
        for (_, dpu_bmc_ip) in &dpu_bmc_ips {
            db::explored_endpoints::set_preingestion_complete(*dpu_bmc_ip, &mut txn)
                .await
                .expect("DPU endpoint preingestion should be marked complete");
        }
        txn.commit()
            .await
            .expect("database transaction should commit");

        self.site_explorer
            .run_single_iteration()
            .await
            .expect("second site explorer iteration should succeed");

        let api = self.test_harness.api_arc();
        let dpus = (0..config.dpus.len())
            .map(|dpu_index| {
                let dpu_index = dpu_index.try_into().expect("DPU index should fit into u8");
                TestDpuMachine::new(
                    *dpu_machine_ids
                        .get(&dpu_index)
                        .expect("DPU machine id should exist"),
                    api.clone(),
                    &config.dpus[dpu_index as usize],
                )
            })
            .collect();
        let managed_host = TestManagedHost {
            host: TestHostMachine::new(host_machine_id(&config), api.clone(), &config),
            dpus,
            api,
        };

        if self.report_dpu_network_status {
            managed_host.report_dpu_network_status().await;
        }

        let build_data = TestManagedHostBuildData {
            host_bmc_ip,
            dpu_bmc_ips: dpu_bmc_ips.iter().map(|(_, ip)| *ip).collect(),
        };

        (managed_host, build_data)
    }
}

fn host_machine_id(config: &ManagedHostConfig) -> HostMachineId {
    if let Some(dpu) = config.dpus.first() {
        return host_id_from_dpu_hardware_info(&HardwareInfo::from(dpu))
            .expect("host machine id should be derived from DPU hardware info")
            .try_into()
            .expect("host exploration report machine ID should be a host machine");
    }

    let mut report: EndpointExplorationReport = config.clone().into();
    (*report
        .generate_machine_id(true)
        .expect("host exploration report should generate a machine id")
        .expect("host exploration report should include a generated machine id"))
    .try_into()
    .expect("host exploration report machine ID should be a host machine")
}

async fn register_expected_machine(test_harness: &TestHarness, managed_host: &ManagedHostConfig) {
    let mut txn = test_harness.db_txn().await;
    db::expected_machine::create(
        &mut txn,
        ExpectedMachine {
            id: None,
            bmc_mac_address: managed_host.bmc_mac_address,
            data: managed_host
                .expected_machine_data
                .clone()
                .unwrap_or_else(|| ExpectedMachineData {
                    serial_number: managed_host.serial.clone(),
                    ..Default::default()
                }),
        },
    )
    .await
    .expect("expected machine should be created");
    txn.commit()
        .await
        .expect("database transaction should commit");
}

async fn discover_bmc(
    api: &Api,
    mac_address: MacAddress,
    segment: TestNetworkSegment,
    vendor_string: &str,
) -> IpAddr {
    api.discover_dhcp(
        DhcpDiscovery::builder(mac_address, segment.relay_address)
            .vendor_string(vendor_string)
            .tonic_request(),
    )
    .await
    .expect("BMC DHCP discovery should succeed")
    .into_inner()
    .address
    .parse()
    .expect("DHCP response address should be an IP address")
}
