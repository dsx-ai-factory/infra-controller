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
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::routing::get;
use axum::{Json, Router};
use bmc_explorer::nv_generate_exploration_report;
use bmc_mock::injection::Action;
use bmc_mock::test_support::axum_http_client::AxumRouterHttpClient;
use bmc_mock::{
    DpuMachineInfo, DpuSettings, HardwareType, HostMachineInfo, MachineInfo, MachineRouterOptions,
    test_support,
};
use model::site_explorer::EndpointType;
use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc};
use serde_json::json;
use tokio::test;
use tokio::time::Instant;

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
    assert_eq!(report.physical_slot_number, Some(26));
    assert_eq!(report.compute_tray_index, Some(16));
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

const GPU_COLLECTION: &str = "/redfish/v1/Systems/HGX_Baseboard_0/Processors";
const GPU_0: &str = "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_0";
const CHASSIS_0: &str = "/redfish/v1/Chassis/Chassis_0";

fn rule(path: &str, action: bmc_mock::injection::Action) -> bmc_mock::injection::Rule {
    bmc_mock::injection::Rule {
        id: path.into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: path.into(),
        },
        action,
        remaining: None,
    }
}

#[test]
async fn partial_cbc_position_keeps_its_valid_field_and_fills_the_partner() {
    let cases = [
        (
            "valid CBC slot",
            json!({"ChassisPhysicalSlotNumber":7}),
            json!({"TraySlotNumber":26,"TraySlotIndex":0}),
            Some(7),
            Some(0),
        ),
        (
            "invalid CBC slot",
            json!({"ChassisPhysicalSlotNumber":"bad","ComputeTrayIndex":0}),
            json!({"TraySlotNumber":26,"TraySlotIndex":16}),
            Some(26),
            Some(0),
        ),
        (
            "invalid CBC tray",
            json!({"ChassisPhysicalSlotNumber":7,"ComputeTrayIndex":-1}),
            json!({"TraySlotNumber":26,"TraySlotIndex":16}),
            Some(7),
            Some(16),
        ),
    ];

    for (case, cbc, gpu, slot, tray) in cases {
        let h = test_support::nvidia_dgx_vr_host_bmc().await;

        h.state.injection.put(vec![
            rule(CHASSIS_0, Action::JsonMerge(json!({"Oem":{"Nvidia":cbc}}))),
            rule(
                GPU_0,
                Action::JsonMerge(json!({"Oem":{"Nvidia":{"MNNVLinkTopology":gpu}}})),
            ),
        ]);

        let report = nv_generate_exploration_report(
            h.bmc.as_ref(),
            h.service_root,
            &common::explorer_config(),
        )
        .await
        .expect(case);

        assert_eq!(
            (report.physical_slot_number, report.compute_tray_index),
            (slot, tray),
            "{case}"
        );
    }
}

#[test(start_paused = true)]
async fn gpu_scan_uses_one_deadline_and_recovers_after_cancellation() {
    let h = test_support::nvidia_dgx_vr_host_bmc().await;

    h.state.injection.put(vec![
        rule(
            CHASSIS_0,
            Action::JsonMerge(json!({"Oem":{"Nvidia":{"ChassisPhysicalSlotNumber":7}}})),
        ),
        rule(
            GPU_COLLECTION,
            Action::Latency {
                mean: Duration::from_secs(35),
                jitter: Duration::ZERO,
            },
        ),
        rule(
            GPU_0,
            Action::Latency {
                mean: Duration::from_secs(35),
                jitter: Duration::ZERO,
            },
        ),
    ]);

    let started = Instant::now();

    let report = nv_generate_exploration_report(
        h.bmc.as_ref(),
        h.service_root.clone(),
        &common::explorer_config(),
    )
    .await
    .expect("optional timeout must preserve inventory");

    assert_eq!(started.elapsed(), Duration::from_secs(60));

    assert_eq!(
        (report.physical_slot_number, report.compute_tray_index),
        (Some(7), None)
    );

    assert_eq!(report.systems[0].id, "System_0");

    h.state.injection.put(vec![rule(
        CHASSIS_0,
        Action::JsonMerge(json!({"Oem":{"Nvidia":{"ChassisPhysicalSlotNumber":7}}})),
    )]);

    let recovered =
        nv_generate_exploration_report(h.bmc.as_ref(), h.service_root, &common::explorer_config())
            .await
            .expect("cancelled reads must release the client for the next exploration");

    assert_eq!(
        (recovered.physical_slot_number, recovered.compute_tray_index),
        (Some(7), Some(16))
    );
}

async fn candidate_bmc(
    reversed: bool,
    first_position: serde_json::Value,
) -> (test_support::TestBmcHandle, Arc<AtomicUsize>) {
    let info = {
        let mut pool = test_support::TEST_MAC_POOL.lock().unwrap();
        let ranges = pool.allocate_range_config().unwrap();
        let dpu = DpuMachineInfo::new(HardwareType::NvidiaDgxVr, &mut pool, DpuSettings::default());
        MachineInfo::Host(HostMachineInfo::new(
            HardwareType::NvidiaDgxVr,
            vec![dpu],
            &mut pool,
            ranges,
        ))
    };

    let (base, state) = bmc_mock::machine_router(
        &info,
        Arc::new(test_support::TestCallbacks::default()),
        "test-host-id".into(),
        false,
        MachineRouterOptions::default(),
    );

    let second_id = format!("{GPU_COLLECTION}/GPU_1");
    let mut members = vec![json!({"@odata.id":GPU_0}), json!({"@odata.id":second_id})];

    if reversed {
        members.reverse();
    }

    let second_reads = Arc::new(AtomicUsize::new(0));
    let reads = second_reads.clone();
    let second_resource_id = second_id.clone();

    state.injection.upsert(rule(
        GPU_0,
        Action::Replace(json!({
            "@odata.id": GPU_0,
            "ProcessorType": "GPU",
            "Oem": {"Nvidia": {"MNNVLinkTopology": first_position}}
        })),
    ));

    let routes = Router::new()
        .route(GPU_COLLECTION, get(move || async move { Json(json!({"@odata.id":GPU_COLLECTION,"Members":members})) }))
        .route(&second_id, get(move || async move {
            reads.fetch_add(1, Ordering::Relaxed);
            Json(json!({"@odata.id":second_resource_id,"ProcessorType":"GPU","Oem":{"Nvidia":{"MNNVLinkTopology":{"TraySlotNumber":27,"TraySlotIndex":17}}}}))
        }))
        .fallback_service(base);

    let bmc = Arc::new(HttpBmc::new(
        AxumRouterHttpClient::new(routes),
        "https://bmc-mock.local".parse().unwrap(),
        BmcCredentials::new("root".into(), "password".into()),
        CacheSettings::with_capacity(32),
    ));

    let service_root = nv_redfish::ServiceRoot::new(bmc.clone())
        .await
        .unwrap()
        .into();

    (
        test_support::TestBmcHandle {
            bmc,
            service_root,
            state,
        },
        second_reads,
    )
}

#[test]
async fn sorted_gpu_selection_stops_without_combining_resources() {
    for reversed in [false, true] {
        let (h, second_reads) = candidate_bmc(reversed, json!({"TraySlotNumber":26})).await;

        if reversed {
            h.state.injection.upsert(rule("/redfish/v1/Systems", Action::JsonMerge(json!({
                "Members":[{"@odata.id":"/redfish/v1/Systems/System_0"},{"@odata.id":"/redfish/v1/Systems/HGX_Baseboard_0"}]
            }))));
        }

        let report = nv_generate_exploration_report(
            h.bmc.as_ref(),
            h.service_root,
            &common::explorer_config(),
        )
        .await
        .expect("stable GPU selection");

        assert_eq!(
            (report.physical_slot_number, report.compute_tray_index),
            (Some(26), None)
        );

        assert_eq!(
            second_reads.load(Ordering::Relaxed),
            0,
            "selection must stop before the disagreeing GPU"
        );

        assert_eq!(
            report.systems[0].id, "System_0",
            "BIOS host selection must remain unchanged"
        );
    }
}

#[test]
async fn failed_gpu_candidate_allows_the_next_candidate_and_later_recovers() {
    let (h, second_reads) =
        candidate_bmc(true, json!({"TraySlotNumber":26,"TraySlotIndex":16})).await;

    let mut failure = rule(GPU_0, Action::Status(503));
    failure.id = "fail-gpu0".into();

    let failure_id = h.state.injection.upsert(failure);

    let report = nv_generate_exploration_report(
        h.bmc.as_ref(),
        h.service_root.clone(),
        &common::explorer_config(),
    )
    .await
    .expect("failure must allow the next GPU candidate");

    assert_eq!(
        (report.physical_slot_number, report.compute_tray_index),
        (Some(27), Some(17))
    );

    assert_eq!(second_reads.load(Ordering::Relaxed), 1);
    assert_eq!(report.systems[0].id, "System_0");
    h.state.injection.delete(&failure_id);

    let recovered =
        nv_generate_exploration_report(h.bmc.as_ref(), h.service_root, &common::explorer_config())
            .await
            .expect("next exploration must retry the previously failed GPU");

    assert_eq!(
        (recovered.physical_slot_number, recovered.compute_tray_index),
        (Some(26), Some(16))
    );

    assert_eq!(
        second_reads.load(Ordering::Relaxed),
        1,
        "recovered GPU0 must stop the scan before GPU1"
    );
}
