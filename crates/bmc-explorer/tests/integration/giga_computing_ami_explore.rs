/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
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

/// A Giga Computing R263-ZG0 runs an AMI MegaRAC BMC, so its ServiceRoot
/// reports `Vendor: AMI` like a generic AMI box. It must classify as
/// `GigaComputingAmi` from `Systems/Self.Manufacturer`, map to
/// `BMCVendor::GigaComputing`, read lockdown from the host interface, and
/// check the Giga BIOS table rather than the generic AMI one.
#[test]
async fn explore_giga_computing_ami() {
    let h = test_support::giga_computing_ami_bmc().await;
    let config = common::explorer_config();

    assert_eq!(
        detect_hw_type(h.service_root.clone(), &config)
            .await
            .unwrap(),
        Some(HwType::GigaComputingAmi),
    );

    let report = nv_generate_exploration_report(h.service_root, &config)
        .await
        .unwrap();
    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert_eq!(report.vendor, Some(bmc_vendor::BMCVendor::GigaComputing));
    assert_eq!(
        report.systems[0].manufacturer.as_deref(),
        Some("Giga Computing")
    );
    assert_eq!(
        report.systems[0].model.as_deref(),
        Some("R263-ZG0-AAL2-000")
    );
    assert!(!report.chassis.is_empty(), "chassis must be present");

    // Factory-open host interface, and the KCS caveat is always appended.
    let lockdown = report
        .lockdown_status
        .expect("Giga must report lockdown status");
    assert_eq!(lockdown.status, InternalLockdownStatus::Disabled);
    assert_eq!(
        lockdown.message,
        "host_interface=true, auth_none=true, cred_bootstrap=true; \
         KCS not exposed via Redfish on this platform (state unknown)"
    );
}
