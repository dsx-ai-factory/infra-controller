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
use bmc_mock::{DpuSettings, test_support};
use model::site_explorer::{BlueFieldOperatingMode, EndpointType};
use tokio::test;

use crate::common;

#[test]
async fn explore_bluefield3_baseline() {
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(DpuSettings::default()).await;
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert_eq!(report.vendor, Some(bmc_vendor::BMCVendor::Nvidia));
    assert!(!report.systems.is_empty(), "systems must be present");
    assert_eq!(report.systems[0].serial_console_ssh_port, Some(2200));
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

#[test]
async fn explore_bluefield3_uses_dynamic_system_console_port_not_manager_ssh() {
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(DpuSettings::default()).await;
    assert!(h.state.has_enabled_ssh_serial_console());
    assert!(h.state.set_serial_console_ssh_port(Some(3222)));
    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "manager_ssh_port".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Managers/Bluefield_BMC/NetworkProtocol".into(),
        },
        action: bmc_mock::injection::Action::JsonMerge(serde_json::json!({
            "SSH": {
                "ProtocolEnabled": true,
                "Port": 22,
            },
        })),
        remaining: None,
    }]);

    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    assert_eq!(report.systems[0].serial_console_ssh_port, Some(3222));
}

#[test]
async fn explore_bluefield3_ignores_invalid_system_interface_mac() {
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(DpuSettings::default()).await;
    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "invalid_system_interface_mac".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Systems/Bluefield/EthernetInterfaces/oob_net0".into(),
        },
        action: bmc_mock::injection::Action::JsonMerge(serde_json::json!({
            "Id": "eth0",
            "InterfaceEnabled": true,
            "LinkStatus": "LinkDown",
            "MACAddress": "00:00:11:e7:fe:80:00:00:00:00:00:00:02:00:00:03:00:18:00:01",
        })),
        remaining: None,
    }]);

    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();
    let system = report.systems.first().expect("systems must be present");
    let eth0 = system
        .ethernet_interfaces
        .iter()
        .find(|interface| interface.id.as_deref() == Some("eth0"))
        .expect("invalid interface must be preserved");
    let oob = system
        .ethernet_interfaces
        .iter()
        .find(|interface| interface.id.as_deref() == Some("oob_net0"))
        .expect("OOB interface must be preserved");

    assert_eq!(eth0.mac_address, None);
    assert!(oob.mac_address.is_some(), "valid OOB MAC must be preserved");
    assert!(system.base_mac.is_some(), "DPU base MAC must be preserved");
    assert!(
        report.dpu_pairing_serial_number().is_some(),
        "DPU pairing serial number must be preserved"
    );
}

#[test]
async fn explore_bluefield3_preserves_oem_mode_and_base_mac() {
    let settings = DpuSettings {
        nic_mode: true,
        ..Default::default()
    };
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(settings).await;
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();
    let system = report.systems.first().expect("systems must be present");

    assert!(system.base_mac.is_some());
    assert_eq!(
        system.attributes.nic_mode,
        Some(BlueFieldOperatingMode::Nic)
    );
}

#[test]
async fn explore_bluefield3_without_system_eth_interfaces() {
    let settings = DpuSettings {
        exposes_oob_eth: false,
        ..Default::default()
    };
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(settings).await;
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();
    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert_eq!(
        report
            .systems
            .first()
            .map(|v| v.ethernet_interfaces.is_empty()),
        Some(true)
    );
}

#[test]
async fn explore_bluefield3_recovers_oob_interface_from_boot_options() {
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(DpuSettings::default()).await;
    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "missing_system_eth_interfaces".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Systems/Bluefield/EthernetInterfaces".into(),
        },
        action: bmc_mock::injection::Action::JsonMerge(serde_json::json!({
            "Members": [],
            "Members@odata.count": 0,
        })),
        remaining: None,
    }]);

    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();
    let system = report.systems.first().expect("systems must be present");

    assert!(system.ethernet_interfaces.iter().any(|interface| {
        interface.id.as_deref() == Some("oob_net0") && interface.mac_address.is_some()
    }));
}

#[test]
async fn explore_bluefield3_retries_transient_404_on_system_eth_interfaces() {
    let settings = DpuSettings::default();

    let h = test_support::dell_poweredge_r750_bluefield3_bmc(settings.clone()).await;
    let baseline =
        nv_generate_exploration_report(h.service_root.clone(), &common::explorer_config())
            .await
            .unwrap();

    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "transient_404".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Systems/Bluefield/EthernetInterfaces".into(),
        },
        action: bmc_mock::injection::Action::Status(404),
        remaining: Some(1),
    }]);

    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    let baseline_count = baseline.systems.first().unwrap().ethernet_interfaces.len();
    let actual_count = report.systems.first().unwrap().ethernet_interfaces.len();
    assert_eq!(actual_count, baseline_count);
}

#[test]
async fn explore_bluefield3_permanent_404_on_system_eth_interfaces_fails_without_hanging() {
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(DpuSettings::default()).await;

    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "permanent_404".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Systems/Bluefield/EthernetInterfaces".into(),
        },
        action: bmc_mock::injection::Action::Status(404),
        remaining: Some(10),
    }]);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        nv_generate_exploration_report(h.service_root, &common::explorer_config()),
    )
    .await;

    let result = result.expect("exploration should terminate and not hang in retry loop");
    assert!(
        result.is_err(),
        "permanent 404 should still fail after retries are exhausted"
    );
}

#[test]
async fn explore_bluefield3_skips_erot_chassis() {
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(DpuSettings::default()).await;
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    let chassis_ids: Vec<&str> = report.chassis.iter().map(|c| c.id.as_str()).collect();
    assert!(
        !chassis_ids.contains(&"Bluefield_ERoT"),
        "Bluefield_ERoT chassis should be skipped, but found chassis: {chassis_ids:?}"
    );
    assert_eq!(
        report.chassis.len(),
        3,
        "expected 3 chassis (Bluefield_BMC, CPU_0, Card1), got: {chassis_ids:?}"
    );
}

#[test]
async fn explore_bluefield3_succeeds_when_erot_hangs() {
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(DpuSettings::default()).await;

    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "erot_hang".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: None,
            glob: "/redfish/v1/Chassis/Bluefield_ERoT".into(),
        },
        action: bmc_mock::injection::Action::Latency {
            mean: std::time::Duration::from_secs(30),
            jitter: std::time::Duration::ZERO,
        },
        remaining: None,
    }]);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        nv_generate_exploration_report(h.service_root, &common::explorer_config()),
    )
    .await;

    let report = result
        .expect("exploration must not hang on ERoT timeout")
        .expect("exploration must succeed");

    let chassis_ids: Vec<&str> = report.chassis.iter().map(|c| c.id.as_str()).collect();
    assert!(
        !chassis_ids.contains(&"Bluefield_ERoT"),
        "Bluefield_ERoT should be skipped even when it would hang"
    );
    assert_eq!(report.chassis.len(), 3);
}

#[test]
async fn explore_bluefield3_succeeds_when_erot_returns_error() {
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(DpuSettings::default()).await;

    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "erot_500".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Chassis/Bluefield_ERoT".into(),
        },
        action: bmc_mock::injection::Action::Status(500),
        remaining: Some(100),
    }]);

    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .expect("exploration must succeed even when ERoT returns 500");

    let chassis_ids: Vec<&str> = report.chassis.iter().map(|c| c.id.as_str()).collect();
    assert!(
        !chassis_ids.contains(&"Bluefield_ERoT"),
        "Bluefield_ERoT should be skipped even when it returns errors"
    );
    assert_eq!(report.chassis.len(), 3);
}

#[test]
async fn explore_bluefield3_ignores_500_on_bios_fetch() {
    let h = test_support::dell_poweredge_r750_bluefield3_bmc(DpuSettings::default()).await;

    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "bios_500".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Systems/Bluefield/Bios".into(),
        },
        action: bmc_mock::injection::Action::Status(500),
        remaining: Some(100),
    }]);

    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .expect("exploration must succeed when BlueField BIOS fetch returns 500");

    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert!(
        report
            .machine_setup_status
            .as_ref()
            .is_some_and(|status| !status.diffs.is_empty() || status.is_done),
        "machine setup status must be present and structurally valid"
    );
}
