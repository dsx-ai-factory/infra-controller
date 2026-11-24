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

use carbide_instrument::testing::capture_logs_async;
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_site_explorer::test_support::{MockEndpointExplorer, TestSiteExplorer};
use carbide_site_explorer::{EndpointExplorationService, SiteExplorer};
use carbide_test_harness::network::segment::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use db::TransactionVending as _;
use mac_address::MacAddress;
use model::expected_switch::{ExpectedNvosInterface, ExpectedSwitch};
use model::metadata::Metadata;
use model::site_explorer::{
    Chassis, ComputerSystem, EndpointExplorationError, EndpointExplorationReport, EndpointType,
    EthernetInterface, ExploredManagedSwitch, PreingestionState,
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

    /// Exercise endpoint protection after the caller has ingested a switch or shelf.
    pub(super) async fn check_ingested_endpoint_rediscovery(
        &self,
        explorer: &TestSiteExplorer,
        address: IpAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let error = self
            .api()
            .delete_explored_endpoint(tonic::Request::new(
                rpc::forge::DeleteExploredEndpointRequest {
                    ip_address: address.to_string(),
                },
            ))
            .await
            .expect_err("an ingested device's endpoint must not be deleted");
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            error.message(),
            format!("cannot delete endpoint {address} because an ingested device owns it")
        );
        let endpoints = db::explored_endpoints::find_all_by_ip(address, &self.pool).await?;
        assert_eq!(
            endpoints.len(),
            1,
            "rejected deletion must retain the endpoint"
        );

        let mut txn = self.pool.txn_begin().await?;
        db::explored_endpoints::delete(&mut txn, address).await?;
        txn.commit().await?;
        explorer.run_single_iteration().await?;

        let endpoints = db::explored_endpoints::find_all_by_ip(address, &self.pool).await?;
        assert_eq!(endpoints.len(), 1, "explorer must recreate the endpoint");
        assert_eq!(endpoints[0].preingestion_state, PreingestionState::Complete);
        assert!(
            !db::explored_endpoints::find_preingest_not_waiting_not_error(&self.pool)
                .await?
                .iter()
                .any(|endpoint| endpoint.address == address),
            "the recreated endpoint must not enter the preingestion work queue"
        );
        Ok(())
    }

    /// An ingested device's failed exploration is recorded without resetting it.
    pub(super) async fn check_ingested_endpoint_remediation(
        &self,
        explorer: &TestSiteExplorer,
        address: IpAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let before = db::explored_endpoints::find_all_by_ip(address, &self.pool)
            .await?
            .pop()
            .expect("the ingested endpoint must exist");
        let error = EndpointExplorationError::RedfishError {
            details: "transient failure on an ingested device".to_string(),
            response_body: None,
            response_code: Some(500),
        };
        explorer.insert_endpoint_result(address, Err(error.clone()));

        let (result, logs) = capture_logs_async(explorer.run_single_iteration()).await;
        result?;
        let address_text = address.to_string();
        let guard_log = logs
            .iter()
            .find(|log| {
                log.message
                    == "Site explorer will not remediate error because this endpoint belongs to an ingested device"
                    && log.field("endpoint") == Some(address_text.as_str())
            })
            .expect("the ingested-device guard must skip remediation for this endpoint");
        assert_eq!(guard_log.field("error"), Some(error.to_string().as_str()));

        let after = db::explored_endpoints::find_all_by_ip(address, &self.pool)
            .await?
            .pop()
            .expect("the failed exploration must retain the ingested endpoint");
        assert_eq!(after.report.last_exploration_error, Some(error));
        assert_eq!(
            after.report_version.version_nr(),
            before.report_version.version_nr() + 1
        );
        assert_eq!(
            (
                after.last_redfish_bmc_reset,
                after.last_ipmitool_bmc_reset,
                after.last_redfish_reboot,
                after.last_redfish_powercycle,
            ),
            (
                before.last_redfish_bmc_reset,
                before.last_ipmitool_bmc_reset,
                before.last_redfish_reboot,
                before.last_redfish_powercycle,
            ),
            "skipping remediation must preserve all reset timestamps"
        );
        assert!(
            explorer
                .endpoint_explorer()
                .redfish_power_control_calls
                .lock()
                .unwrap()
                .is_empty(),
            "an ingested device must not receive a power-control request"
        );
        Ok(())
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
        Default::default(),
        api.common_pools().clone(),
        api.work_lock_manager_handle(),
        api.runtime_config.rack_profiles.clone(),
        None,
        api.credential_manager().clone(),
        false,
        api.instrumentation().clone(),
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
