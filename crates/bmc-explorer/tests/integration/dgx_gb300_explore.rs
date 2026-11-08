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
use bmc_explorer::hw::HwType;
use bmc_explorer::nv_generate_exploration_report;
use bmc_explorer::test_support::detect_hw_type;
use bmc_mock::test_support;
use model::site_explorer::EndpointType;
use tokio::test;

use crate::common;

#[test]
async fn gb300_assembly_serial_reaches_report_and_ambiguity_fails() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Json, Router, routing::get};
    use bmc_mock::injection::{Action, Rule, Selector};
    use bmc_mock::test_support::axum_http_client::AxumRouterHttpClient;
    use bmc_mock::{DpuMachineInfo, DpuSettings, HardwareType, HostMachineInfo, MachineInfo};
    use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc};

    const ASSEMBLY: &str = "/redfish/v1/Chassis/Chassis_0/Assembly";

    for (scenario, second_serial, ambiguous) in [
        ("unprogrammed second tray FRU", "999XXXXXXXXXX", false),
        ("conflicting second tray FRU", "9990000000003", true),
    ] {
        let machine_info = {
            let mut pool = test_support::TEST_MAC_POOL.lock().unwrap();
            let ranges = pool.allocate_range_config().unwrap();
            let dpu = DpuMachineInfo::new(
                HardwareType::NvidiaDgxGb300,
                &mut pool,
                DpuSettings::default(),
            );
            MachineInfo::Host(HostMachineInfo::new(
                HardwareType::NvidiaDgxGb300,
                vec![dpu],
                &mut pool,
                ranges,
            ))
        };
        let (router, state) = bmc_mock::machine_router(
            &machine_info,
            Arc::new(test_support::TestCallbacks::default()),
            "test-host-id".into(),
            false,
            Default::default(),
        );
        // This profile has no assembly inventory. Add both its advertised link
        // and a successful endpoint; JSON injection cannot replace its 404.
        state.injection.upsert(Rule {
            id: "gb300-tray-assembly-link".into(),
            selector: Selector::OdataId("/redfish/v1/Chassis/Chassis_0".into()),
            action: Action::JsonMerge(serde_json::json!({
                "Assembly": {"@odata.id": ASSEMBLY}
            })),
            remaining: Some(1),
        });
        let assembly = serde_json::json!({
            "@odata.id": ASSEMBLY,
            "@odata.type": "#Assembly.v1_3_0.Assembly",
            "Id": "Assembly", "Name": "Chassis assembly",
            "Assemblies": [
                {
                    "MemberId": "0", "Name": "Tray FRU",
                    "Model": "GB300 Titania-Bianca Compute Tray",
                    "SerialNumber": "9990000000001", "PartNumber": "675-20000-0000-TEST"
                },
                {
                    "MemberId": "1", "Name": "Board FRU", "Model": "P5859 B03",
                    "SerialNumber": "9990000000002", "PartNumber": "699-20000-0000-TEST"
                },
                {
                    "MemberId": "2", "Name": "Secondary tray FRU",
                    "Model": "GB300 Titania-Bianca Compute Tray",
                    "SerialNumber": second_serial, "PartNumber": "965-XXXXX-XXXX-XXX"
                }
            ]
        });
        let assembly_requests = Arc::new(AtomicUsize::new(0));
        let requests = assembly_requests.clone();
        let router = Router::new()
            .route(
                ASSEMBLY,
                get(move || {
                    requests.fetch_add(1, Ordering::Relaxed);
                    let assembly = assembly.clone();
                    async move { Json(assembly) }
                }),
            )
            .fallback_service(router);
        let bmc = Arc::new(HttpBmc::new(
            AxumRouterHttpClient::new(router),
            "https://bmc.example".parse().unwrap(),
            BmcCredentials::new("root".into(), "password".into()),
            CacheSettings::with_capacity(32),
        ));
        let root = nv_redfish::ServiceRoot::new(bmc)
            .await
            .unwrap()
            .restrict_expand()
            .into();
        let report = nv_generate_exploration_report(root, &common::explorer_config()).await;
        if ambiguous {
            assert!(
                matches!(report, Err(bmc_explorer::Error::InvalidValue(ref error))
                    if error == "multiple distinct GB300 tray assembly serial numbers"),
                "{scenario}: {report:?}"
            );
        } else {
            let report = report.unwrap();
            assert_eq!(
                report
                    .chassis
                    .iter()
                    .find(|chassis| chassis.id == "Chassis_0")
                    .unwrap()
                    .serial_number
                    .as_deref(),
                Some("9990000000001"),
                "{scenario}"
            );
        }
        assert!(
            state.injection.list().is_empty(),
            "{scenario}: assembly link"
        );
        assert_eq!(assembly_requests.load(Ordering::Relaxed), 1, "{scenario}");
    }
}

/// A DGX GB300 runs the NVIDIA "GB BMC" -- the same ServiceRoot signature as a
/// Wiwynn GB200 (`Vendor: NVIDIA`, `Product: GB BMC`). It must classify as the
/// GB300 platform (resolved from the NVIDIA GB300 GPU chassis ahead of the GB200
/// arm), not as `Gb200`. `DgxGb300` maps to BMCVendor::Nvidia.
#[test]
async fn explore_dgx_gb300() {
    let h = test_support::dgx_gb300_bmc().await;
    let config = common::explorer_config();

    // Decisive assertion: a DGX GB300 must resolve to DgxGb300, not the Gb200
    // fallback. Both map to BMCVendor::Nvidia, so asserting the report vendor
    // alone would pass even with the DgxGb300 arm removed.
    assert_eq!(
        detect_hw_type(h.service_root.clone(), &config)
            .await
            .unwrap(),
        Some(HwType::DgxGb300),
    );

    let report = nv_generate_exploration_report(h.service_root, &config)
        .await
        .unwrap();
    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert_eq!(report.vendor, Some(bmc_vendor::BMCVendor::Nvidia));
    // The class is derived from what the BMC reports about the system, so it
    // names this tray where the report vendor cannot.
    assert_eq!(
        report.hardware_class.as_deref(),
        Some("nvidia_gb300-titania-bianca-compute-tray")
    );
    assert!(!report.systems.is_empty(), "systems must be present");
    assert!(!report.chassis.is_empty(), "chassis must be present");
    // The tray advertises a ComponentIntegrity collection, so the report
    // carries every member the BMC listed, in the order it listed them.
    assert_eq!(
        report
            .component_integrities
            .as_deref()
            .map(|entries| entries
                .iter()
                .map(|entry| (
                    entry.id.as_str(),
                    entry.component_integrity_type.as_str(),
                    entry.component_integrity_enabled
                ))
                .collect::<Vec<_>>()),
        Some(vec![
            ("ERoT_BMC_0", "SPDM", true),
            ("HGX_ERoT_CPU_0", "SPDM", true),
            ("HGX_ERoT_CPU_1", "SPDM", true),
            ("HGX_ERoT_GPU_0", "SPDM", true),
            ("HGX_ERoT_GPU_1", "SPDM", true),
            ("HGX_ERoT_GPU_2", "SPDM", true),
            ("HGX_ERoT_GPU_3", "SPDM", true),
        ]),
    );
}

#[test]
async fn explore_dgx_gb300_skips_erot_bmc_chassis() {
    let h = test_support::dgx_gb300_bmc().await;
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    let chassis_ids: Vec<&str> = report.chassis.iter().map(|c| c.id.as_str()).collect();
    assert!(
        !chassis_ids.contains(&"ERoT_BMC_0"),
        "ERoT_BMC_0 chassis should be skipped, but found chassis: {chassis_ids:?}"
    );
    assert!(chassis_ids.contains(&"Chassis_0"), "{chassis_ids:?}");
}

#[test]
async fn explore_dgx_gb300_succeeds_when_erot_bmc_hangs() {
    let h = test_support::dgx_gb300_bmc().await;

    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "erot_hang".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: None,
            glob: "/redfish/v1/Chassis/ERoT_BMC_0".into(),
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

    result
        .expect("exploration must not hang on ERoT timeout")
        .expect("exploration must succeed");
}

#[test]
async fn explore_dgx_gb300_succeeds_when_erot_bmc_returns_error() {
    let h = test_support::dgx_gb300_bmc().await;

    h.state.injection.put(vec![bmc_mock::injection::Rule {
        id: "erot_500".into(),
        selector: bmc_mock::injection::Selector::Path {
            method: Some("GET".into()),
            glob: "/redfish/v1/Chassis/ERoT_BMC_0".into(),
        },
        action: bmc_mock::injection::Action::Status(500),
        remaining: Some(100),
    }]);

    nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .expect("exploration must succeed even when ERoT returns 500");
}
