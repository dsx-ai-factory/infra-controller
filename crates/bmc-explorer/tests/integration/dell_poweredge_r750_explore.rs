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
use serde_json::{Value, json};
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
    // This iDRAC advertises no ComponentIntegrity collection, which the report
    // records as absent rather than as one reported empty.
    assert_eq!(report.component_integrities, None);
    assert!(!report.component_integrity_unavailable);
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
    assert!(
        report
            .machine_setup_status
            .as_ref()
            .is_some_and(|status| { status.diffs.iter().any(|diff| diff.key == "BootSeqRetry") }),
        "disabled BootSeqRetry must be reported as a setup mismatch"
    );
}

const STORAGE: &str = "/redfish/v1/Systems/System.Embedded.1/Storage";

fn storage(id: &str, drive_paths: &[&str]) -> Value {
    json!({
        "@odata.id": format!("{STORAGE}/{id}"),
        "@odata.type": "#Storage.v1_16_0.Storage",
        "Id": id,
        "Name": id,
        "Drives": drive_paths.iter().map(|path| json!({ "@odata.id": path })).collect::<Vec<_>>()
    })
}

fn hdd(path: &str, state: &str) -> Value {
    let id = path.rsplit('/').next().unwrap();
    json!({
        "@odata.id": path,
        "@odata.type": "#Drive.v1_19_0.Drive",
        "Id": id,
        "Name": id,
        "MediaType": "HDD",
        "Model": "TOSHIBA MG11ACA2",
        "Revision": "GR02",
        "Status": { "State": state }
    })
}

#[test]
async fn explore_dell_poweredge_r750_reports_drive_firmware_when_enabled() {
    let bay0 = format!("{STORAGE}/NonRAID.Slot.5-1/Drives/Disk.Bay.0");
    let bay1 = format!("{STORAGE}/NonRAID.Slot.5-1/Drives/Disk.Bay.1");
    let unserved = format!("{STORAGE}/BOSS.SL.14-1/Drives/Disk.Direct.0-0");
    let storages = vec![
        storage("NonRAID.Slot.5-1", &[&bay0, &bay1]),
        // A second Storage linking the same drive must not duplicate its entry.
        storage("AHCI.Embedded.1-1", &[&bay0]),
        // A Storage whose drive cannot be fetched is skipped, not fatal.
        storage("BOSS.SL.14-1", &[&unserved]),
    ];
    let drives = vec![hdd(&bay0, "Enabled"), hdd(&bay1, "Absent")];

    for (explore_drive_firmware, expected) in [
        (false, vec![]),
        (
            true,
            vec![("Drive_TOSHIBA MG11ACA2_Disk.Bay.0", Some("GR02"))],
        ),
    ] {
        let h =
            test_support::dell_poweredge_r750_bmc_with_storage(storages.clone(), drives.clone())
                .await;
        let config = bmc_explorer::Config {
            explore_drive_firmware,
            ..common::explorer_config()
        };
        let report = nv_generate_exploration_report(h.service_root, &config)
            .await
            .unwrap();

        let drive_entries = report
            .service
            .iter()
            .filter(|service| service.id == "FirmwareInventory")
            .flat_map(|service| &service.inventories)
            .filter(|inventory| inventory.id.starts_with("Drive_"))
            .map(|inventory| (inventory.id.as_str(), inventory.version.as_deref()))
            .collect::<Vec<_>>();
        assert_eq!(
            drive_entries, expected,
            "explore_drive_firmware = {explore_drive_firmware}"
        );
    }
}
