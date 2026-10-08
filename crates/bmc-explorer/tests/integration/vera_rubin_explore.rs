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
    assert_eq!(report.systems[0].processors, None);
    let hgx = &report.systems[1];
    assert_eq!(hgx.manufacturer.as_deref(), Some("NVIDIA"));
    assert_eq!(hgx.model.as_deref(), Some("VR NVL"));
    assert_eq!(hgx.serial_number, None);
    assert_eq!(hgx.processors.as_ref().unwrap().len(), 1);
    assert_eq!(hgx.processors.as_ref().unwrap()[0].id, "GPU_0");
    assert_eq!(report.rack_position().physical_slot_number, Some(26));
    assert_eq!(report.rack_position().compute_tray_index, Some(16));
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
    report.parse_position_info();
    assert_eq!(report.physical_slot_number, Some(26));
    assert_eq!(report.compute_tray_index, Some(16));

    let mut refreshed_report =
        nv_generate_exploration_report(h.bmc.as_ref(), h.service_root, &config)
            .await
            .expect("subsequent NvidiaDgxVr host exploration should succeed");
    assert_eq!(refreshed_report.systems, report.systems);
    refreshed_report.parse_position_info();
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
        [
            "Bios",
            "EthernetInterfaces",
            "BootOptions",
            "PCIeDevices",
            "SecureBoot",
        ]
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
            "Boot": {
                "BootOrder": ["Boot0001"],
                "BootOptions": {"@odata.id": "/redfish/v1/Systems/HGX_Baseboard_0/BootOptions"}
            },
            "SecureBoot": {"@odata.id": "/redfish/v1/Systems/HGX_Baseboard_0/SecureBoot"},
            "SerialNumber": " secondary-serial ",
            "BiosVersion": " secondary-bios "
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
        report.systems[1].serial_number.as_deref(),
        Some("secondary-serial")
    );
    assert_eq!(
        report.systems[1].bios_version.as_deref(),
        Some("secondary-bios")
    );
    assert_eq!(report.systems[1].boot_order, None);
    assert_eq!(report.systems[1].processors.as_ref().unwrap().len(), 1);
    assert_eq!(
        h.state.injection.list().len(),
        5,
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

#[test]
async fn unavailable_processors_do_not_prevent_system_discovery() {
    use bmc_mock::injection::{Action, Rule, Selector};
    let h = test_support::nvidia_dgx_vr_host_bmc().await;
    h.state.injection.upsert(Rule {
        id: "unavailable-processors".into(),
        selector: Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Systems/HGX_Baseboard_0/Processors*".into(),
        },
        action: Action::Status(500),
        remaining: Some(1),
    });
    let service_root = h.service_root.as_ref().clone().restrict_expand().into();
    let mut report =
        nv_generate_exploration_report(h.bmc.as_ref(), service_root, &common::explorer_config())
            .await
            .unwrap();
    assert_eq!(report.systems[0].id, "System_0");
    assert_eq!(report.systems[1].id, "HGX_Baseboard_0");
    assert_eq!(report.systems[1].processors, Some(vec![]));
    assert!(
        h.state.injection.list().is_empty(),
        "processor collection must have been requested"
    );
    assert_eq!(report.rack_position(), Default::default());
    assert!(report.generate_machine_id(true).unwrap().is_some());
}

#[test]
async fn non_vera_rubin_systems_do_not_fetch_processors() {
    use bmc_mock::injection::{Action, Rule, Selector};
    let h = test_support::nvidia_dgx_h100_bmc().await;
    h.state.injection.upsert(Rule {
        id: "processors-not-requested".into(),
        selector: Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Systems/*/Processors*".into(),
        },
        action: Action::Status(500),
        remaining: Some(1),
    });
    let root = h.service_root.as_ref().clone().restrict_expand().into();
    let report = nv_generate_exploration_report(h.bmc.as_ref(), root, &common::explorer_config())
        .await
        .unwrap();
    assert!(
        report
            .systems
            .iter()
            .any(|system| system.id == "HGX_Baseboard_0")
    );
    assert!(
        report
            .systems
            .iter()
            .all(|system| system.processors.is_none())
    );
    assert_eq!(h.state.injection.list()[0].remaining, Some(1));
}

#[test]
async fn vera_rubin_collects_all_processors_and_selects_tray_gpu_from_report() {
    use bmc_mock::injection::{Action, Rule, Selector};
    use serde_json::json;
    let h = test_support::nvidia_dgx_vr_host_bmc().await;
    let members = ["GPU_1", "GPU_0", "GPU_0"].map(|id| {
        json!({
            "@odata.id": format!("/redfish/v1/Systems/HGX_Baseboard_0/Processors/{id}"),
            "@odata.type": "#Processor.v1_20_0.Processor", "Id": id, "Name": id,
            "ProcessorType": "GPU", "Oem": {"Nvidia": {
                "@odata.type": "#NvidiaProcessor.v1_4_0.NvidiaGPU", "MNNVLinkTopology": {
                "TraySlotNumber": 26, "TraySlotIndex": if id == "GPU_0" { 16 } else { 99 }
            }}}
        })
    });
    h.state.injection.put(vec![
        Rule {
            id: "primary-processors-link".into(),
            selector: Selector::OdataId("/redfish/v1/Systems/System_0".into()),
            action: Action::JsonMerge(json!({
                "Processors": {"@odata.id": "/redfish/v1/Systems/System_0/Processors"}
            })),
            remaining: Some(1),
        },
        Rule {
            id: "primary-processors-not-requested".into(),
            selector: Selector::Path {
                method: Some("GET".into()),
                glob: "/redfish/v1/Systems/System_0/Processors*".into(),
            },
            action: Action::Status(500),
            remaining: Some(1),
        },
        Rule {
            id: "processor-inventory".into(),
            selector: Selector::OdataId("/redfish/v1/Systems/HGX_Baseboard_0/Processors".into()),
            action: Action::Replace(json!({
                "@odata.id": "/redfish/v1/Systems/HGX_Baseboard_0/Processors",
                "@odata.type": "#ProcessorCollection.ProcessorCollection",
                "Name": "Processors", "Members@odata.count": 3,
                "Members": members
            })),
            remaining: Some(1),
        },
    ]);
    let root = h.service_root.as_ref().clone().restrict_expand().into();
    let report = nv_generate_exploration_report(h.bmc.as_ref(), root, &common::explorer_config())
        .await
        .unwrap();
    assert_eq!(report.systems[0].processors, None);
    let processors = report.systems[1].processors.as_ref().unwrap();
    assert_eq!(processors.len(), 2);
    assert_eq!(processors[0].id, "GPU_0");
    assert_eq!(processors[1].id, "GPU_1");
    assert_eq!(processors[1].compute_tray_index, Some(99));
    assert_eq!(report.rack_position().compute_tray_index, Some(16));
    let pending = h.state.injection.list();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, "primary-processors-not-requested".into());
    assert_eq!(pending[0].remaining, Some(1));
}
