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

/// Regression coverage for the NvidiaDgxVr (Vera Rubin) host mock, added while
/// investigating #3159. This hardware type previously had no host-mode test
/// helper at all (only a DPU-mode one), so it was untested as a host machine.
#[test]
async fn explore_nvidia_dgx_vr_and_generate_machine_id() {
    let h = test_support::nvidia_dgx_vr_host_bmc().await;
    let config = common::explorer_config();

    let mut report = nv_generate_exploration_report(h.bmc.as_ref(), h.service_root, &config)
        .await
        .expect("NvidiaDgxVr host exploration should succeed");

    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert!(!report.systems.is_empty(), "systems must be present");
    assert_eq!(report.systems[0].serial_console_ssh_port, Some(2200));
    assert!(!report.chassis.is_empty(), "chassis must be present");
    assert!(
        report.systems[0].pcie_devices.is_empty(),
        "VR host pairing should use the BlueField chassis inventory, not host PCIe devices"
    );

    let bluefield_chassis = report
        .chassis
        .iter()
        .find(|chassis| chassis.id == "BlueField_0")
        .expect("VR host report should expose the attached BF4 as BlueField_0 chassis");
    assert_eq!(
        bluefield_chassis.part_number.as_deref(),
        Some("900-9D4A4-00CB-TS4")
    );
    assert!(
        bluefield_chassis
            .serial_number
            .as_deref()
            .is_some_and(|serial| !serial.is_empty()),
        "BlueField_0 chassis should carry the DPU serial for host/DPU pairing"
    );
    assert!(
        bluefield_chassis
            .network_adapters
            .iter()
            .any(|adapter| adapter.id == "BlueField_NIC_0"),
        "BlueField_0 chassis should expose the real VR BlueField_NIC_0 adapter path"
    );

    let machine_id = report
        .generate_machine_id(true)
        .expect("NvidiaDgxVr host report should have enough data for a MachineId")
        .expect("NvidiaDgxVr host report should generate a predicted-host MachineId");

    assert!(
        machine_id.machine_type().is_predicted_host(),
        "expected a PredictedHost machine type for a non-DPU tray"
    );
}
