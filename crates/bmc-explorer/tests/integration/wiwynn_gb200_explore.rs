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
use tokio::test;

use crate::common;

#[test]
async fn chassis_assemblies_preserve_board_and_tray_serials_on_new_firmware() {
    let h = test_support::wiwynn_gb200_bmc().await;
    let chassis = h.service_root.chassis().await.unwrap().unwrap();
    let members = chassis.members().await.unwrap();
    let chassis = members
        .iter()
        .find(|chassis| chassis.raw().id == "Chassis_0")
        .unwrap();
    let board_serial = chassis.hardware_id().serial_number.unwrap().to_string();
    let assembly = chassis.assembly().await.unwrap().unwrap();
    let assemblies = assembly.assemblies().await.unwrap();
    assert_eq!(
        assemblies[0].hardware_id().model.unwrap().into_inner(),
        "Titania-Bianca Compute Tray PDB"
    );
    let tray_serial = assemblies[0]
        .hardware_id()
        .serial_number
        .unwrap()
        .to_string();
    assert_ne!(tray_serial, board_serial);
    assert_eq!(
        assemblies[1]
            .hardware_id()
            .serial_number
            .unwrap()
            .to_string(),
        board_serial
    );

    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();
    assert_eq!(
        report
            .systems
            .iter()
            .find(|system| system.id == "System_0")
            .unwrap()
            .serial_number
            .as_deref(),
        Some(board_serial.as_str())
    );
    // This is the serde representation written to explored_endpoints.exploration_report.
    let json = serde_json::to_value(&report).unwrap();
    let saved = json["Chassis"].as_array().unwrap();
    let chassis = saved
        .iter()
        .find(|chassis| chassis["Id"] == "Chassis_0")
        .unwrap();
    assert_eq!(chassis["SerialNumber"], board_serial);
    assert_eq!(
        chassis["Assemblies"],
        serde_json::json!([
            {"MemberId": "0", "Name": "PDB Chassis FRU Assembly0",
             "Model": "Titania-Bianca Compute Tray PDB", "SerialNumber": tray_serial},
            {"MemberId": "1", "Name": "PDB Board FRU Assembly1",
             "Model": "P5859 B01", "SerialNumber": board_serial}
        ])
    );
    assert!(
        saved
            .iter()
            .filter(|c| c["Id"] != "Chassis_0")
            .all(|c| c.get("Assemblies").is_none())
    );
}

#[test]
async fn chassis_assemblies_follow_the_configured_chassis_id_pattern() {
    use bmc_mock::injection::{Action, Rule, Selector};
    use carbide_hw_identification::pattern::Pattern;

    // Advertise the same Assembly on two chassis so the report identifies which link was followed.
    const ASSEMBLY: &str = "/redfish/v1/Chassis/Chassis_0/Assembly";
    for (pattern, expected) in [
        (Pattern::Prefix("bmc_".into()), &["BMC_0"][..]),
        (Pattern::Contains("_0".into()), &["BMC_0", "Chassis_0"]),
        (Pattern::Exact("missing".into()), &[]),
    ] {
        let h = test_support::wiwynn_gb200_bmc().await;
        h.state.injection.upsert(Rule {
            id: "bmc-assembly-link".into(),
            selector: Selector::OdataId("/redfish/v1/Chassis/BMC_0".into()),
            action: Action::JsonMerge(serde_json::json!({"Assembly": {"@odata.id": ASSEMBLY}})),
            remaining: None,
        });
        let config = bmc_explorer::Config {
            collect_chassis_assembly: &|chassis| pattern.matches(&chassis.id),
            ..common::explorer_config()
        };
        let root = h.service_root.as_ref().clone().restrict_expand().into();
        let report = nv_generate_exploration_report(root, &config).await.unwrap();
        let mut collected: Vec<_> = report
            .chassis
            .iter()
            .filter(|chassis| !chassis.assemblies.is_empty())
            .map(|chassis| chassis.id.as_str())
            .collect();
        collected.sort_unstable();
        assert_eq!(collected, expected);
        let assembly_reads = h
            .http_client
            .take_requests()
            .iter()
            .filter(|req| req.uri.contains(ASSEMBLY))
            .count();
        assert_eq!(assembly_reads, expected.len());
    }
}

#[test]
async fn chassis_position_remains_the_source_for_gb200() {
    let h = test_support::wiwynn_gb200_bmc_at_rack_position(11).await;
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    let cbc = report
        .chassis
        .iter()
        .find(|chassis| chassis.id.starts_with("CBC_"))
        .expect("GB200 compute tray must expose a CBC chassis");
    assert_eq!(cbc.physical_slot_number, Some(10));
    assert_eq!(cbc.compute_tray_index, Some(0));

    let position = report.rack_position();
    assert_eq!(position.physical_slot_number, Some(10));
    assert_eq!(position.compute_tray_index, Some(0));
}
