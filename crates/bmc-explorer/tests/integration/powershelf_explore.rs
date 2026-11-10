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
use bmc_explorer::{Error, nv_generate_exploration_report};
use bmc_mock::injection::{Action, Rule, Selector};
use bmc_mock::test_support;
use model::site_explorer::{EndpointType, PowerState};
use serde_json::json;
use tokio::test;

use crate::common;

#[test]
async fn explore_liteon_power_shelf() {
    for (name, psu_power, expected) in [
        ("on", json!(true), PowerState::On),
        ("off", json!(false), PowerState::Off),
        ("missing PSU state", json!(null), PowerState::Unknown),
    ] {
        let h = test_support::liteon_powershelf_bmc().await;
        assert!(h.service_root.root.systems.is_none());
        h.state.injection.put(vec![
            Rule {
                id: "systems-must-not-be-requested".into(),
                selector: Selector::Path {
                    method: Some("GET".into()),
                    glob: "/redfish/v1/Systems*".into(),
                },
                action: Action::Status(500),
                remaining: Some(1),
            },
            Rule {
                id: "psu-power".into(),
                selector: Selector::OdataId(
                    "/redfish/v1/Chassis/powershelf/PowerSubsystem/PowerSupplies/*".into(),
                ),
                action: Action::JsonMerge(json!({"PowerState": psu_power})),
                remaining: None,
            },
        ]);
        let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
            .await
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        assert_eq!(report.vendor, Some(bmc_vendor::BMCVendor::Liteon), "{name}");
        assert_eq!(report.systems.len(), 1, "{name}");
        let system = &report.systems[0];
        assert_eq!(system.id, "powershelf", "{name}");
        assert_eq!(
            system.manufacturer.as_deref(),
            Some("LITE-ON TECHNOLOGY CORP."),
            "{name}"
        );
        assert_eq!(system.model.as_deref(), Some("PF-1333-7R"), "{name}");
        assert_eq!(system.power_state, expected, "{name}");
        assert_eq!(report.chassis.len(), 1, "{name}");
        let chassis = &report.chassis[0];
        assert_eq!(chassis.id, "powershelf", "{name}");
        assert!(chassis.serial_number.is_some(), "{name}");
        assert_eq!(system.serial_number, chassis.serial_number, "{name}");
        assert_eq!(report.managers.len(), 1, "{name}");
        assert!(
            report
                .service
                .iter()
                .any(|service| service.id == "FirmwareInventory"),
            "{name}"
        );
        assert!(
            h.state.injection.list().iter().any(|rule| {
                rule.id == "systems-must-not-be-requested".into() && rule.remaining == Some(1)
            }),
            "{name}: /Systems was requested"
        );
    }
}

#[test]
async fn explore_server_without_systems_fails() {
    let h = test_support::dell_poweredge_r750_bmc().await;
    h.state.injection.upsert(Rule {
        id: "missing-systems-link".into(),
        selector: Selector::OdataId("/redfish/v1".into()),
        action: Action::JsonMerge(json!({"Systems": null})),
        remaining: None,
    });
    let root = nv_redfish::ServiceRoot::new(h.bmc).await.unwrap();
    let result = nv_generate_exploration_report(root.into(), &common::explorer_config()).await;
    assert!(
        matches!(result, Err(Error::BmcNotProvided("systems"))),
        "{result:?}"
    );
}

// Delta power shelves expose no `/redfish/v1/Systems` collection (the service
// root omits the `Systems` link and the endpoint 404s). Site-explorer must
// detect the Delta chassis and synthesize a `ComputerSystem` from it instead of
// failing on the missing collection -- this is the ingestion regression the
// Delta support fixes. The mock (`delta_powershelf_bmc`) reproduces the missing
// collection, so a successful report here guards that path.
#[test]
async fn explore_delta_power_shelf() {
    let h = test_support::delta_powershelf_bmc().await;
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    assert_eq!(report.endpoint_type, EndpointType::Bmc);
    assert_eq!(report.vendor, Some(bmc_vendor::BMCVendor::Delta));
    assert_eq!(report.systems[0].id, "chassis");
    assert_eq!(report.systems[0].manufacturer.as_deref(), Some("DELTA"));
    assert_eq!(report.systems[0].serial_console_ssh_port, None);
    assert!(
        !report.systems.is_empty(),
        "a system must be synthesized from the Delta chassis despite no Systems collection"
    );
    // The mock reports every Delta PSU as `Oem.deltaenergysystems.Power: true`,
    // so the synthesized system's power state must resolve to On. This exercises
    // the typed `oem_delta()` accessor end-to-end (extension parse -> per-PSU
    // flag -> aggregated shelf state).
    assert!(
        report
            .systems
            .iter()
            .all(|system| system.power_state == PowerState::On),
        "synthesized Delta system power state must be On"
    );
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

// When every Delta PSU reports `Power: false`, the aggregated shelf state must
// resolve to Off (all-off => Off), exercising the non-On path end-to-end.
#[test]
async fn explore_delta_power_shelf_all_off() {
    let h = test_support::delta_powershelf_bmc_with_psu_power(vec![false; 6]).await;
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    assert!(!report.systems.is_empty(), "a system must be synthesized");
    assert!(
        report
            .systems
            .iter()
            .all(|system| system.power_state == PowerState::Off),
        "an all-off Delta shelf must resolve to Off"
    );
}

// A mix of on and off PSUs is not a coherent shelf state, so the aggregation
// must collapse to Unknown rather than guessing On or Off.
#[test]
async fn explore_delta_power_shelf_mixed() {
    let h = test_support::delta_powershelf_bmc_with_psu_power(vec![
        true, false, true, false, true, false,
    ])
    .await;
    let report = nv_generate_exploration_report(h.service_root, &common::explorer_config())
        .await
        .unwrap();

    assert!(!report.systems.is_empty(), "a system must be synthesized");
    assert!(
        report
            .systems
            .iter()
            .all(|system| system.power_state == PowerState::Unknown),
        "a mixed Delta shelf must resolve to Unknown"
    );
}
