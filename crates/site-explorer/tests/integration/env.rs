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

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_site_explorer::test_support::{MockEndpointExplorer, TestSiteExplorer};
use carbide_site_explorer::{EndpointExplorationService, SiteExplorer};
use carbide_test_harness::network::segment::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use mac_address::MacAddress;
use model::expected_switch::{ExpectedNvosInterface, ExpectedSwitch};
use model::metadata::Metadata;
use model::site_explorer::{
    Chassis, ComputerSystem, EndpointExplorationReport, EndpointType, EthernetInterface,
    ExploredManagedSwitch,
};

/// Keep this shared env to the setup common to most site-explorer tests.
/// Tests that need extra domains, segments, or other one-off objects should
/// create those objects locally instead of adding fields here.
pub(super) struct Env {
    pub(super) pool: PgPool,
    pub(super) underlay_segment: TestNetworkSegment,
    pub(super) test_harness: TestHarness,
}

impl Env {
    pub(super) async fn new(pool: PgPool) -> Self {
        let test_harness = TestHarness::builder(pool.clone()).build().await;
        let domain = test_harness.test_domain().await;
        let nc = test_harness.network_controller();
        let underlay_segment = nc.create_underlay_segment(&domain).await;
        Self {
            pool,
            underlay_segment,
            test_harness,
        }
    }

    pub(super) fn api(&self) -> &Api {
        self.test_harness.api()
    }

    pub(super) fn test_site_explorer(
        &self,
        explorer_config: SiteExplorerConfig,
    ) -> TestSiteExplorer {
        test_site_explorer(&self.test_harness, explorer_config)
    }
}

pub(super) fn test_site_explorer(
    test_harness: &TestHarness,
    explorer_config: SiteExplorerConfig,
) -> TestSiteExplorer {
    let endpoint_explorer = Arc::new(MockEndpointExplorer::default());
    let api = test_harness.api();
    let endpoint_exploration_service = Arc::new(EndpointExplorationService::new(
        api.database_connection.clone(),
        endpoint_explorer.clone(),
        Arc::new(api.runtime_config.get_firmware_config()),
    ));
    let site_explorer = SiteExplorer::new(
        api.database_connection.clone(),
        explorer_config,
        test_harness.test_meter.meter(),
        endpoint_exploration_service,
        endpoint_explorer.clone(),
        api.common_pools().clone(),
        api.work_lock_manager_handle(),
        api.runtime_config.rack_profiles.clone(),
        None,
        api.credential_manager().clone(),
        false,
    );
    TestSiteExplorer::new(site_explorer, endpoint_explorer)
}

/// An expected switch record whose BMC and NVOS MAC addresses and serial are
/// the given ones.
pub(super) fn expected_switch_fixture(
    bmc_mac: MacAddress,
    nvos_mac: MacAddress,
    serial: &str,
) -> ExpectedSwitch {
    ExpectedSwitch {
        expected_switch_id: None,
        bmc_mac_address: bmc_mac,
        nvos_interfaces: vec![ExpectedNvosInterface {
            mac_address: nvos_mac,
            is_primary: true,
        }],
        serial_number: serial.to_string(),
        bmc_username: "ADMIN".to_string(),
        bmc_password: "Pwd2023".to_string(),
        nvos_username: None,
        nvos_password: None,
        bmc_ip_address: None,
        nvos_ip_address: None,
        metadata: Metadata {
            name: format!("Test Switch {serial}"),
            description: String::new(),
            labels: HashMap::new(),
        },
        rack_id: None,
        bmc_retain_credentials: None,
    }
}

/// An explored NVIDIA switch whose report carries the switch chassis with
/// `chassis_serial` and one NVOS interface with `nvos_mac`; the report alone
/// is what an explored endpoint stores.
pub(super) fn explored_managed_switch_fixture(
    bmc_ip: IpAddr,
    nvos_mac: MacAddress,
    chassis_serial: Option<&str>,
) -> ExploredManagedSwitch {
    let chassis = Chassis {
        id: "mgx_nvswitch_0".to_string(),
        manufacturer: Some("NVIDIA".to_string()),
        model: Some("Switch".to_string()),
        serial_number: chassis_serial.map(String::from),
        part_number: chassis_serial.map(String::from),
        ..Default::default()
    };
    ExploredManagedSwitch {
        bmc_ip,
        report: EndpointExplorationReport {
            endpoint_type: EndpointType::Bmc,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            chassis: vec![chassis],
            systems: vec![ComputerSystem {
                ethernet_interfaces: vec![EthernetInterface {
                    mac_address: Some(nvos_mac),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            model: Some("Switch".to_string()),
            ..Default::default()
        },
    }
}
