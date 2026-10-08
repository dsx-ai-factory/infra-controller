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

    let mut report =
        nv_generate_exploration_report(h.bmc.as_ref(), h.service_root.clone(), &config)
            .await
            .expect("NvidiaDgxVr host exploration should succeed");

    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert_eq!(
        report
            .systems
            .iter()
            .map(|system| system.id.as_str())
            .collect::<Vec<_>>(),
        ["System_0", "HGX_Baseboard_0"]
    );
    let hgx = &report.systems[1];
    assert_eq!(hgx.manufacturer.as_deref(), Some("NVIDIA"));
    assert_eq!(hgx.model.as_deref(), Some("VR NVL"));
    assert_eq!(hgx.serial_number, None);
    assert!(hgx.ethernet_interfaces.is_empty());
    assert!(hgx.pcie_devices.is_empty());
    assert_eq!(hgx.boot_order, None);
    assert_eq!(hgx.base_mac, None);
    assert_ne!(report.systems[0].serial_number, None);
    let encoded = serde_json::to_string(&report).unwrap();
    assert_eq!(
        serde_json::from_str::<model::site_explorer::EndpointExplorationReport>(&encoded).unwrap(),
        report
    );
    assert_eq!(report.systems[0].serial_console_ssh_port, Some(2200));
    assert_eq!(report.physical_slot_number, Some(26));
    assert_eq!(report.compute_tray_index, Some(16));

    let refreshed_report = nv_generate_exploration_report(h.bmc.as_ref(), h.service_root, &config)
        .await
        .expect("subsequent NvidiaDgxVr host exploration should succeed");
    assert_eq!(refreshed_report.systems, report.systems);
    assert_eq!(refreshed_report.physical_slot_number, Some(26));
    assert_eq!(refreshed_report.compute_tray_index, Some(16));
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

#[test]
async fn additional_systems_do_not_fetch_linked_inventory() {
    use bmc_mock::injection::{Action, Rule, Selector};
    let h = test_support::nvidia_dgx_vr_host_bmc().await;
    h.state.injection.put(
        ["Bios", "EthernetInterfaces", "BootOptions", "PCIeDevices"]
            .into_iter()
            .map(|resource| Rule {
                id: resource.into(),
                selector: Selector::Path {
                    method: Some("GET".into()),
                    glob: format!("/redfish/v1/Systems/HGX_Baseboard_0/{resource}*"),
                },
                action: Action::Status(500),
                remaining: Some(1),
            })
            .collect(),
    );
    h.state.injection.upsert(Rule {
        id: "additional-system-links".into(),
        selector: Selector::OdataId("/redfish/v1/Systems/HGX_Baseboard_0".into()),
        action: Action::JsonMerge(serde_json::json!({
            "Bios": {"@odata.id": "/redfish/v1/Systems/HGX_Baseboard_0/Bios"},
            "EthernetInterfaces": {"@odata.id": "/redfish/v1/Systems/HGX_Baseboard_0/EthernetInterfaces"},
            "Boot": {"BootOptions": {"@odata.id": "/redfish/v1/Systems/HGX_Baseboard_0/BootOptions"}}
        })),
        remaining: Some(1),
    });
    // Fetch members individually so the advertised links pass through injection.
    let service_root = h.service_root.as_ref().clone().restrict_expand().into();
    let report =
        nv_generate_exploration_report(h.bmc.as_ref(), service_root, &common::explorer_config())
            .await
            .unwrap();
    assert_eq!(report.systems[1].id, "HGX_Baseboard_0");
    assert_eq!(
        h.state.injection.list().len(),
        4,
        "the resource link patch must have been consumed"
    );
    assert!(
        h.state
            .injection
            .list()
            .iter()
            .all(|rule| rule.remaining == Some(1))
    );
}
