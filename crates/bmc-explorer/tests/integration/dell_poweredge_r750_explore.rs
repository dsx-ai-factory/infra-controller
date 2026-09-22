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
use bmc_explorer::nv_generate_exploration_report;
use bmc_mock::test_support;
use model::site_explorer::EndpointType;
use tokio::test;

use crate::common;

#[test]
async fn explore_dell_poweredge_r750() {
    let h = test_support::dell_poweredge_r750_bmc().await;
    assert!(!h.state.has_enabled_ssh_serial_console());
    assert!(!h.state.set_serial_console_ssh_port(Some(3222)));
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert_eq!(report.vendor, Some(bmc_vendor::BMCVendor::Dell));
    assert!(!report.systems.is_empty(), "systems must be present");
    assert_eq!(report.systems[0].serial_console_ssh_port, None);
    assert!(!report.chassis.is_empty(), "chassis must be present");
    assert!(
        report
            .service
            .iter()
            .any(|service| service.id == "FirmwareInventory"),
        "firmware inventory service must be present"
    );
    assert!(
        report
            .machine_setup_status
            .as_ref()
            .is_some_and(|status| !status.diffs.is_empty() || status.is_done),
        "machine setup status must be present and structurally valid"
    );
}
