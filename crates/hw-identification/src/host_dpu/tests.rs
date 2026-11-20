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

use DpuProduct::{BlueField3Dpu, BlueField3SuperNic9D3B4, BlueField4Dpu};
use model::site_explorer::{ComputerSystem, NetworkAdapter, PCIeDevice};

use super::*;
use crate::hw::nvidia_bluefield_3::{DPU_9D3B6_00CV_AA0 as BLUEFIELD, SUPERNIC_9D3B4_00EN_EA0};

fn report(devices: &[(Option<&str>, Option<&str>)]) -> EndpointExplorationReport {
    EndpointExplorationReport {
        systems: devices
            .iter()
            .map(|(part_number, serial_number)| ComputerSystem {
                pcie_devices: vec![PCIeDevice {
                    description: None,
                    firmware_version: None,
                    gpu_vendor: None,
                    id: None,
                    manufacturer: None,
                    name: None,
                    part_number: part_number.map(str::to_string),
                    serial_number: serial_number.map(str::to_string),
                    status: None,
                }],
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn chassis(
    part_number: Option<&str>,
    serial_number: Option<&str>,
    adapters: &[(Option<&str>, Option<&str>)],
) -> Chassis {
    Chassis {
        part_number: part_number.map(str::to_string),
        serial_number: serial_number.map(str::to_string),
        network_adapters: adapters
            .iter()
            .map(|(part_number, serial_number)| NetworkAdapter {
                part_number: part_number.map(str::to_string),
                serial_number: serial_number.map(str::to_string),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

#[test]
fn identifies_card_inventory_reported_by_a_dpu_bmc() {
    let mut inventory = report(&[(Some(BLUEFIELD), Some("reported-card"))]);
    inventory.systems[0].id = "Bluefield".into();
    assert!(inventory.is_dpu());

    let rules = HostDpuRules::default();
    let cards = rules.identify_dpus(&inventory);
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    assert_eq!(card.serial_number, Some("reported-card"));
    assert_eq!(card.rule.product, BlueField3Dpu);
}

#[test]
fn chassis_devices_with_unique_serials() {
    struct Case {
        name: &'static str,
        pcie_devices: &'static [(Option<&'static str>, Option<&'static str>)],
        chassis: Vec<Chassis>,
        serials: &'static [(&'static str, DpuProduct)],
    }

    for case in [
        Case {
            name: "empty inventory",
            pcie_devices: &[],
            chassis: vec![],
            serials: &[],
        },
        Case {
            name: "a BlueField chassis with a serial supplies its own identity",
            pcie_devices: &[],
            chassis: vec![chassis(
                Some(BLUEFIELD),
                Some(" CHASSIS "),
                &[(Some(BLUEFIELD), Some("ADAPTER"))],
            )],
            serials: &[("CHASSIS", BlueField3Dpu)],
        },
        Case {
            name: "a non-BlueField chassis supplies its network adapters",
            pcie_devices: &[],
            chassis: vec![chassis(
                Some("OTHER-PART"),
                Some("CHASSIS"),
                &[(Some(BLUEFIELD), Some("ADAPTER"))],
            )],
            serials: &[("ADAPTER", BlueField3Dpu)],
        },
        Case {
            name: "a BlueField chassis without a usable serial supplies its adapters",
            pcie_devices: &[],
            chassis: vec![chassis(
                Some(BLUEFIELD),
                Some("  "),
                &[(Some(BLUEFIELD), Some("ADAPTER"))],
            )],
            serials: &[("ADAPTER", BlueField3Dpu)],
        },
        Case {
            name: "duplicate serials retain a BlueField part number across chassis",
            pcie_devices: &[],
            chassis: vec![
                chassis(
                    None,
                    None,
                    &[
                        (None, Some(" DPU-A ")),
                        (None, Some("DPU-A")),
                        (None, Some(" DPU-B ")),
                        (None, Some("DPU-B")),
                    ],
                ),
                chassis(Some(BLUEFIELD), Some("DPU-A"), &[]),
            ],
            serials: &[("DPU-A", BlueField3Dpu)],
        },
        Case {
            name: "PCIe and chassis share serials and omit records without usable serials",
            pcie_devices: &[
                (Some(BLUEFIELD), Some("UNEXPLORED")),
                (Some(BLUEFIELD), None),
            ],
            chassis: vec![chassis(
                None,
                None,
                &[
                    (Some(BLUEFIELD), Some(" UNEXPLORED ")),
                    (None, Some("READY")),
                    (Some(BLUEFIELD), Some("READY")),
                    (Some(BLUEFIELD), None),
                ],
            )],
            serials: &[("UNEXPLORED", BlueField3Dpu), ("READY", BlueField3Dpu)],
        },
        Case {
            name: "distinct PCIe and chassis serials are combined in source order",
            pcie_devices: &[(Some(BLUEFIELD), Some(" PCIE "))],
            chassis: vec![chassis(None, None, &[(None, Some("CHASSIS"))])],
            serials: &[("PCIE", BlueField3Dpu)],
        },
        Case {
            name: "PCIe wins when both sources have conflicting BlueField part numbers",
            pcie_devices: &[(Some(SUPERNIC_9D3B4_00EN_EA0), Some(" DPU "))],
            chassis: vec![chassis(Some(BLUEFIELD), Some("DPU"), &[])],
            serials: &[("DPU", BlueField3SuperNic9D3B4)],
        },
        Case {
            name: "chassis supplies the BlueField part number missing from PCIe",
            pcie_devices: &[(None, Some(" DPU "))],
            chassis: vec![chassis(Some(BLUEFIELD), Some("DPU"), &[])],
            serials: &[("DPU", BlueField3Dpu)],
        },
    ] {
        let mut report = report(case.pcie_devices);
        report.chassis = case.chassis;
        let serials = HostDpuRules::default()
            .identify_dpus(&report)
            .into_iter()
            .filter_map(|device| Some((device.serial_number?, device.rule.product)))
            .collect::<Vec<_>>();
        assert_eq!(serials, case.serials, "{}: serials", case.name);
    }
}

#[test]
fn pcie_devices_with_unique_serials() {
    struct Case {
        name: &'static str,
        devices: &'static [(Option<&'static str>, Option<&'static str>)],
        serials: &'static [(&'static str, DpuProduct)],
    }

    for case in [
        Case {
            name: "empty inventory",
            devices: &[],
            serials: &[],
        },
        Case {
            name: "unknown parts cannot identify a DPU by themselves",
            devices: &[
                (Some("OTHER-PART"), Some(" OTHER-SERIAL ")),
                (None, Some("OTHER-SERIAL")),
                (None, Some(" SERIAL-ONLY ")),
                (None, Some("SERIAL-ONLY")),
            ],
            serials: &[],
        },
        Case {
            name: "duplicates across systems retain the BlueField part number",
            devices: &[
                (None, Some(" DPU-A ")),
                (Some(BLUEFIELD), Some("DPU-A")),
                (Some(BLUEFIELD), Some("DPU-B")),
                (None, Some("DPU-B")),
            ],
            serials: &[("DPU-A", BlueField3Dpu), ("DPU-B", BlueField3Dpu)],
        },
        Case {
            name: "devices without usable serials are omitted",
            devices: &[
                (Some(BLUEFIELD), None),
                (Some(BLUEFIELD), Some("")),
                (Some(BLUEFIELD), Some("  ")),
                (Some("OTHER-PART"), None),
            ],
            serials: &[],
        },
    ] {
        let report = report(case.devices);
        let serials = HostDpuRules::default()
            .identify_dpus(&report)
            .into_iter()
            .filter_map(|device| Some((device.serial_number?, device.rule.product)))
            .collect::<Vec<_>>();
        assert_eq!(serials, case.serials, "{}: serials", case.name);
    }
}

#[test]
fn site_rules_extend_defaults_only_at_their_declared_sources() {
    #[derive(Deserialize, Serialize)]
    struct Config {
        host_dpu_rules: HostDpuRules,
    }
    let config: Config = toml::from_str(
        r#"
        [[host_dpu_rules]]
        id = "site-oem-bf4"
        sources = ["chassis_network_adapter"]
        part_numbers = [{exact = "OEM-X"}, {prefix = "ALIAS-"}]
        product = "bluefield4_dpu"
    "#,
    )
    .unwrap();
    let encoded = toml::to_string(&config).unwrap();
    let saved: Config = toml::from_str(&encoded).unwrap();
    assert_eq!(saved.host_dpu_rules, config.host_dpu_rules);

    let mut host = report(&[
        (Some("OEM-X"), Some("wrong-source")),
        (Some(" sn37c16116 "), Some(" 123 ")),
    ]);
    host.chassis = vec![chassis(
        None,
        None,
        &[
            (Some(" oem-x "), Some(" 456 ")),
            (Some("ALIAS-SKU"), None),
            (Some("OEM-X-more"), Some("wrong-exact")),
        ],
    )];
    let cards = config.host_dpu_rules.identify_dpus(&host);
    let observed = cards
        .iter()
        .map(|card| {
            (
                card.serial_number,
                card.part_number,
                card.pcie_device,
                card.rule.product,
                card.rule.id.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        [
            (
                Some("123"),
                " sn37c16116 ",
                Some(&host.systems[1].pcie_devices[0]),
                BlueField3Dpu,
                "lenovo-thinksystem-bluefield3-dpu"
            ),
            (Some("456"), " oem-x ", None, BlueField4Dpu, "site-oem-bf4"),
            (None, "ALIAS-SKU", None, BlueField4Dpu, "site-oem-bf4"),
        ]
    );
    let site_rule = cards[1].rule;
    assert!(std::ptr::eq(cards[2].rule, site_rule));
    drop(host);
    assert!(std::ptr::eq(site_rule, &config.host_dpu_rules.0[0]));
}

#[test]
fn invalid_site_rules_are_rejected_at_load_time() {
    #[derive(Deserialize)]
    struct Config {
        #[serde(rename = "rules")]
        _rules: HostDpuRules,
    }
    for rule in [
        r#"{sources=["chassis"], part_numbers=[{exact="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="", sources=["chassis"], part_numbers=[{exact="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="nvidia-bluefield4-dpu", sources=["chassis"], part_numbers=[{exact="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="site-test", sources=[], part_numbers=[{exact="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="site-test", sources=["chassis"], part_numbers=[], product="bluefield3_dpu"}"#,
        r#"{id="site-test", sources=["chassis"], part_numbers=[{contains=" "}], product="bluefield3_dpu"}"#,
        r#"{id="site-test", sources=["misspelled"], part_numbers=[{exact="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="site-test", sources=["chassis"], part_numbers=[{regex="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="site-test", sources=["chassis"], part_numbers=[{exact="PN"}], product="misspelled"}"#,
    ] {
        assert!(
            toml::from_str::<Config>(&format!("rules = [{rule}]")).is_err(),
            "{rule}"
        );
    }
}

#[test]
fn invalid_rule_id_reports_its_index_in_the_site_rules() {
    let site_rule = |id: &'static str| HostDpuRule {
        id: id.into(),
        ..DEFAULT_HOST_DPU_RULES[0].clone()
    };
    for (rules, index) in [
        (vec![site_rule("")], 0),
        (vec![site_rule("site-card"), site_rule(" padded ")], 1),
    ] {
        let error = HostDpuRules::try_from(rules).unwrap_err();
        assert!(error.contains(&format!("at index {index}:")), "{error}");
    }
}

#[test]
fn repeated_site_rule_ids_are_rejected_even_for_different_patterns() {
    #[derive(Debug, Deserialize)]
    struct Config {
        #[serde(rename = "rules")]
        _rules: HostDpuRules,
    }
    let error = toml::from_str::<Config>(r#"
        rules = [
          {id="site-card", sources=["chassis"], part_numbers=[{exact="A"}], product="bluefield2_dpu"},
          {id="site-card", sources=["chassis"], part_numbers=[{exact="B"}], product="bluefield4_dpu"},
        ]
    "#).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("duplicate host DPU rule ID \"site-card\"")
    );
}

#[test]
fn builtin_pcie_products() {
    use DpuProduct::*;
    let rules = HostDpuRules::default();
    for (part, expected) in [
        (" MBF2H516A-CENOT ", Some(BlueField2Dpu)),
        (BLUEFIELD, Some(BlueField3Dpu)),
        (" SN37B36732 ", Some(BlueField3Dpu)),
        ("sn37c16116", Some(BlueField3Dpu)),
        ("SN37B36732XYZ", None),
        (SUPERNIC_9D3B4_00EN_EA0, Some(BlueField3SuperNic9D3B4)),
        ("900-9D3D4-00EN-HA0_Ax", Some(BlueField3SuperNic)),
        ("900-9D4B4-CWAA-TSA", Some(BlueField4Dpu)),
        ("900-9D4A4-00CB-TS4", Some(BlueField4Dpu)),
        ("NVIDIA BlueField mystery board", Some(UnspecifiedBlueField)),
        ("", None),
    ] {
        assert_eq!(
            rules
                .matching_rule(InventorySource::SystemPcieDevice, part)
                .map(|r| r.product),
            expected,
            "{part:?}"
        );
    }
}
