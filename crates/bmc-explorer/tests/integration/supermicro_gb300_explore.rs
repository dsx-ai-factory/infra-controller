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
use bmc_explorer::hw::HwType;
use bmc_explorer::nv_generate_exploration_report;
use bmc_explorer::test_support::detect_hw_type;
use bmc_mock::test_support;
use model::site_explorer::EndpointType;
use tokio::test;

use crate::common;

/// An SMC GB300 runs a Supermicro OpenBMC host BMC, so its ServiceRoot reports
/// `Vendor: Supermicro` / `Product: GB NVL` with no OEM key. It must classify as
/// the GB300 platform (resolved from the NVIDIA GB300 GPU chassis, then routed by
/// the Supermicro vendor string), and `SupermicroGb300` maps to BMCVendor::Supermicro.
#[test]
async fn explore_supermicro_gb300() {
    let h = test_support::supermicro_gb300_bmc().await;
    h.state.manager.set_ipmi_endpoint(Some(1623));
    let config = common::explorer_config();

    // Decisive assertion: an SMC GB300 must resolve to SupermicroGb300, not the
    // generic Supermicro fallback. Both map to BMCVendor::Supermicro, so a report
    // vendor assertion alone would not prove the SupermicroGb300 arm was taken.
    assert_eq!(
        detect_hw_type(h.service_root.clone(), &config)
            .await
            .unwrap(),
        Some(HwType::SupermicroGb300),
    );

    let report = nv_generate_exploration_report(h.service_root, &config)
        .await
        .unwrap();
    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert_eq!(report.vendor, Some(bmc_vendor::BMCVendor::Supermicro));
    assert_eq!(
        report.managers.first().and_then(|m| m.ipmi_port),
        Some(1623)
    );
    assert!(!report.systems.is_empty(), "systems must be present");
    assert_eq!(report.systems[0].serial_console_ssh_port, Some(2200));
    assert!(!report.chassis.is_empty(), "chassis must be present");

    let setup = report
        .machine_setup_status
        .expect("SMC GB300 must report machine setup status");
    assert!(!setup.is_done);
    assert_eq!(
        setup
            .diffs
            .iter()
            .map(|diff| {
                (
                    diff.key.as_str(),
                    diff.expected.as_str(),
                    diff.actual.as_str(),
                )
            })
            .collect::<Vec<_>>(),
        [
            ("Socket0Pcie6DisableOptionROM", "false", "true"),
            ("Socket1Pcie6DisableOptionROM", "false", "true"),
        ]
    );
}

fn answer_chassis_with(h: &test_support::TestBmcHandle, chassis_id: &str, status: u16) {
    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "listed-chassis-status".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: format!("/redfish/v1/Chassis/{chassis_id}"),
        },
        action: bmc_mock::injection::Action::Status(status),
        remaining: None,
    }]);
}

/// Supermicro lists the RAID storage enclosure in the chassis collection while
/// the host is rebooting, but fetching it returns 404 until the controller is
/// up. That chassis must be skipped for the cycle rather than failing the whole
/// exploration, which would raise an allocation-blocking exploration alert.
#[test]
async fn supermicro_skips_listed_chassis_that_returns_404() {
    let config = common::explorer_config();
    let baseline = nv_generate_exploration_report(
        test_support::supermicro_gb300_bmc().await.service_root,
        &config,
    )
    .await
    .unwrap();
    let missing = baseline
        .chassis
        .last()
        .expect("the mock lists chassis")
        .id
        .clone();

    // A fresh BMC, so no response from the baseline run can be reused.
    let h = test_support::supermicro_gb300_bmc().await;
    answer_chassis_with(&h, &missing, 404);
    let report = nv_generate_exploration_report(h.service_root, &config)
        .await
        .expect("a listed chassis that answers 404 must not fail exploration");

    assert_eq!(report.chassis.len(), baseline.chassis.len() - 1);
    assert!(report.chassis.iter().all(|c| c.id != missing));
}

/// Only 404 is tolerated: any other error on a listed chassis still fails.
#[test]
async fn supermicro_listed_chassis_server_error_still_fails_exploration() {
    let config = common::explorer_config();
    let baseline = nv_generate_exploration_report(
        test_support::supermicro_gb300_bmc().await.service_root,
        &config,
    )
    .await
    .unwrap();
    let chassis_id = baseline.chassis.last().unwrap().id.clone();

    let h = test_support::supermicro_gb300_bmc().await;
    answer_chassis_with(&h, &chassis_id, 500);
    assert!(
        nv_generate_exploration_report(h.service_root, &config)
            .await
            .is_err()
    );
}
