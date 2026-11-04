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
use model::site_explorer::{EndpointType, InternalLockdownStatus};
use tokio::test;

use crate::common;

/// A Lenovo GB300 (AMI BMC) must classify as the GB300 platform regardless of the
/// host BMC vendor. GB300 is detected from the NVIDIA "NVIDIA GB300" GPU chassis
/// (`is_gb300()`), so this also guards the detection decouple: the platform is
/// resolved up front from the HGX signature, with the ODM only selecting the variant.
#[test]
async fn explore_lenovo_gb300() {
    let h = test_support::lenovo_gb300_bmc().await;
    let config = common::explorer_config();

    // Decisive assertion: resolves to LenovoGb300 (the GB300 platform), proving
    // GB300 was recognized on an AMI host. Passes before and after the decouple.
    assert_eq!(
        detect_hw_type(h.service_root.clone(), &config)
            .await
            .unwrap(),
        Some(HwType::LenovoGb300),
    );

    let report = nv_generate_exploration_report(h.service_root, &config)
        .await
        .unwrap();
    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    // LenovoGb300 -> BMCVendor::LenovoAMI; proves GB300 was recognized on an AMI host.
    assert_eq!(report.vendor, Some(bmc_vendor::BMCVendor::LenovoAMI));
    assert!(!report.systems.is_empty(), "systems must be present");
    assert!(!report.chassis.is_empty(), "chassis must be present");

    let system_0 = report
        .systems
        .iter()
        .find(|system| system.id == "System_0")
        .expect("Lenovo GB300 must expose the host System_0 resource");
    assert_eq!(
        system_0.model.as_deref(),
        Some("HG635N_V2"),
        "System_0 must report the Lenovo GB300 host model"
    );
    assert_eq!(
        system_0.bios_version.as_deref(),
        Some("GBHC01A_01.05.0"),
        "System_0 must report the host BIOS/UEFI version"
    );
    assert_eq!(
        report.observed_host_bmc_version(),
        Some("1.0.0"),
        "the representative BMC version must be read from the exact BMC inventory entry"
    );
    assert_eq!(
        report.system_bios_version(),
        Some("GBHC01A_01.05.0"),
        "the observed host BIOS accessor must select System_0"
    );

    let inventory = report.get_inventory_map();
    assert!(
        inventory.contains_key("BMC"),
        "Lenovo GB300 firmware inventory must contain the exact BMC entry"
    );
    assert!(
        !inventory.contains_key("UEFI"),
        "Lenovo GB300 firmware inventory must not synthesize a UEFI entry"
    );

    let lockdown = report
        .lockdown_status
        .expect("GB300 lockdown status must be populated");
    assert_eq!(lockdown.status, InternalLockdownStatus::Disabled);
}
