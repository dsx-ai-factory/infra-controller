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

/// A DGX GB300 runs the NVIDIA "GB BMC" -- the same ServiceRoot signature as a
/// Wiwynn GB200 (`Vendor: NVIDIA`, `Product: GB BMC`). It must classify as the
/// GB300 platform (resolved from the NVIDIA GB300 GPU chassis ahead of the GB200
/// arm), not as `Gb200`. `DgxGb300` maps to BMCVendor::Nvidia.
#[test]
async fn explore_dgx_gb300() {
    let h = test_support::dgx_gb300_bmc().await;
    let config = common::explorer_config();

    // Decisive assertion: a DGX GB300 must resolve to DgxGb300, not the Gb200
    // fallback. Both map to BMCVendor::Nvidia, so asserting the report vendor
    // alone would pass even with the DgxGb300 arm removed.
    assert_eq!(
        detect_hw_type(h.service_root.clone(), &config)
            .await
            .unwrap(),
        Some(HwType::DgxGb300),
    );

    let report = nv_generate_exploration_report(h.service_root, &config)
        .await
        .unwrap();
    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert_eq!(report.vendor, Some(bmc_vendor::BMCVendor::Nvidia));
    // The recorded class names the resolved hardware, so it distinguishes a
    // DGX GB300 from a GB200 where the report vendor cannot.
    assert_eq!(report.hardware_class.as_deref(), Some("DgxGb300"));
    assert!(!report.systems.is_empty(), "systems must be present");
    assert!(!report.chassis.is_empty(), "chassis must be present");
}
