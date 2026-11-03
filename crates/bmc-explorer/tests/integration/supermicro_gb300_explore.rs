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
