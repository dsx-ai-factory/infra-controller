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
async fn chassis_serial_comes_from_tray_assembly_on_new_firmware() {
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
    assert_eq!(
        report
            .chassis
            .iter()
            .find(|chassis| chassis.id == "Chassis_0")
            .unwrap()
            .serial_number
            .as_deref(),
        Some(tray_serial.as_str())
    );
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
