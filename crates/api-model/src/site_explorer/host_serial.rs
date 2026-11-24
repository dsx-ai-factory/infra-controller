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

//! Host inventory serial selection for ExpectedMachine audits.

use carbide_hw_identification::host_serial::{HostSerialRules, HostSerialSource};
use carbide_hw_identification::pattern::Pattern;

use super::EndpointExplorationReport;

fn usable_serial(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn matches_field(pattern: Option<&Pattern>, value: Option<&str>) -> bool {
    pattern.is_none_or(|pattern| value.is_some_and(|v| pattern.matches(v)))
}

impl EndpointExplorationReport {
    fn first_usable_serial<'a>(
        &self,
        mut sources: impl Iterator<Item = &'a HostSerialSource>,
    ) -> Option<&str> {
        sources.find_map(|source| match source {
            HostSerialSource::SystemSerial {} => self
                .primary_system()?
                .serial_number
                .as_deref()
                .and_then(usable_serial),
            HostSerialSource::SystemSku {} => self
                .primary_system()?
                .sku
                .as_deref()
                .and_then(usable_serial),
            HostSerialSource::Chassis { chassis_id } => self
                .chassis
                .iter()
                .filter(|chassis| chassis_id.matches(&chassis.id))
                .find_map(|chassis| chassis.serial_number.as_deref().and_then(usable_serial)),
            HostSerialSource::ChassisAssembly {
                chassis_id,
                name,
                model,
                member_id,
            } => self
                .chassis
                .iter()
                .filter(|chassis| chassis_id.matches(&chassis.id))
                .flat_map(|chassis| &chassis.assemblies)
                .filter(|assembly| {
                    matches_field(name.as_ref(), assembly.name.as_deref())
                        && matches_field(model.as_ref(), assembly.model.as_deref())
                        && matches_field(member_id.as_ref(), Some(&assembly.member_id))
                })
                .find_map(|assembly| assembly.serial_number.as_deref().and_then(usable_serial)),
        })
    }

    /// A usable site override is authoritative. Otherwise the primary-system serial
    /// and SKU remain valid alongside the built-in tray or fallback chassis serial.
    pub fn matches_expected_serial_number(&self, expected: &str, rules: &HostSerialRules) -> bool {
        let is_host = !(self.is_dpu() || self.is_switch() || self.is_power_shelf());
        let class = self.hardware_class.as_deref();
        if is_host && let Some(serial) = self.first_usable_serial(rules.site_sources(class)) {
            return serial == expected;
        }
        if self
            .primary_system()
            .is_some_and(|s| s.check_serial_number(expected) || s.check_sku(expected))
        {
            return true;
        }
        if is_host
            && let Some(serial) = self.first_usable_serial(HostSerialRules::built_in_sources(class))
        {
            return serial == expected;
        }
        self.chassis.iter().any(|chassis| {
            let serial = if is_host {
                chassis.serial_number.as_deref().map(str::trim)
            } else {
                chassis.inventory_serial_number()
            };
            serial == Some(expected)
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use serde_json::{Value, json};

    use super::*;
    use crate::site_explorer::{Chassis, ComputerSystem};

    fn report(chassis_id: &str, assemblies: Value) -> EndpointExplorationReport {
        EndpointExplorationReport {
            hardware_class: Some("acme_tray".into()),
            systems: vec![ComputerSystem {
                serial_number: Some("SYSTEM".into()),
                sku: Some("SKU".into()),
                ..Default::default()
            }],
            chassis: vec![
                serde_json::from_value(json!({
                    "Id": chassis_id, "SerialNumber": "BOARD", "Assemblies": assemblies
                }))
                .unwrap(),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn chassis_fru_identifies_the_host_when_product_and_board_share_its_model() {
        let rules = HostSerialRules::default();
        for (name, model) in [
            (
                "PDB Chassis FRU Assembly0",
                "Titania-Bianca Compute Tray PDB",
            ),
            ("PDB Chassis FRU Assembly0", "GB200 NVL"),
        ] {
            let report = report(
                "Chassis_0",
                json!([
                    {"MemberId":"2","Name":"PDB Product FRU Assembly2","Model":model,"SerialNumber":"163XXXXXXXXXX"},
                    {"MemberId":"1","Name":"PDB Board FRU Assembly1","Model":model,"SerialNumber":"BOARD"},
                    {"MemberId":"0","Name":name,"Model":model,"SerialNumber":" TRAY "}
                ]),
            );
            value_scenarios!(run = |expected: &str| report.matches_expected_serial_number(expected, &rules);
                "only the chassis FRU identifies the tray" {
                    "TRAY" => true,
                    "163XXXXXXXXXX" => false,
                    "BOARD" => false,
                }
            );
        }
    }

    #[test]
    fn vera_rubin_firmware_variants_match_the_tray_serial_with_default_rules() {
        let rules = HostSerialRules::default();
        value_scenarios!(run = |assemblies: Value| {
            report("Chassis_0", assemblies).matches_expected_serial_number("TRAY", &rules)
        };
            "VRB-2601-28.00" {
                json!([
                    {"MemberId":"0","Name":"PDB Chassis FRU Assembly0","Model":"P4107","SerialNumber":"TRAY"}
                ]) => true,
            }
            "VRB-2605-16.00 with the same model on the board FRU" {
                json!([
                    {"MemberId":"1","Name":"PDB_0 Board FRU Assembly1","Model":"P4107","SerialNumber":"BOARD"},
                    {"MemberId":"0","Name":"PDB_0 Chassis FRU Assembly0","Model":"P4107","SerialNumber":"TRAY"}
                ]) => true,
            }
        );
    }

    #[test]
    fn site_rules_choose_the_first_usable_source_for_the_hardware_class() {
        let rules: HostSerialRules = serde_json::from_value(json!([
            {"id":"other", "hardware_class":{"exact":"other"}, "source":{"kind":"system_serial"}},
            {"id":"missing", "source":{"kind":"chassis", "chassis_id":{"exact":"Missing"}}},
            {"id":"assembly", "hardware_class":{"prefix":"ACME_"}, "source":{
                "kind":"chassis_assembly","chassis_id":{"exact":"Chassis_0"},"name":{"prefix":"tray"},
                "model":{"contains":"beta"},"member_id":{"prefix":"slot-"}
            }},
            {"id":"fallback", "source":{"kind":"system_sku"}}
        ]))
        .unwrap();
        let mut report = report(
            "Chassis_0",
            json!([
                {"MemberId":"slot-1","Name":"board","Model":"beta","SerialNumber":"WRONG-NAME"},
                {"MemberId":"3","Name":"tray","Model":"beta","SerialNumber":"WRONG-ID"},
                {"MemberId":"slot-2","Name":"tray","Model":"alpha","SerialNumber":"WRONG-MODEL"},
                {"MemberId":"slot-3","Name":"Tray FRU","Model":"OEM BETA","SerialNumber":"  "},
                {"MemberId":"slot-4","Name":"Tray FRU","Model":"OEM BETA","SerialNumber":"SITE"},
                {"MemberId":"0","Name":"PDB Chassis FRU Assembly0","Model":"GB200 NVL","SerialNumber":"BUILT-IN"}
            ]),
        );
        assert!(report.matches_expected_serial_number("SITE", &rules));
        assert!(!report.matches_expected_serial_number("BUILT-IN", &rules));
        report.hardware_class = None;
        assert!(report.matches_expected_serial_number("SKU", &rules));
        assert!(!report.matches_expected_serial_number("SYSTEM", &rules));
        report.hardware_class = Some("other".into());
        assert!(report.matches_expected_serial_number("SYSTEM", &rules));
        assert!(!report.matches_expected_serial_number("SKU", &rules));
        report.systems[0].serial_number = None;
        report.systems[0].sku = Some("  ".into());
        assert!(report.matches_expected_serial_number("BUILT-IN", &rules));
    }

    #[test]
    fn chassis_patterns_use_the_first_usable_serial_across_matching_chassis() {
        let report: EndpointExplorationReport = serde_json::from_value(json!({
            "EndpointType":"Bmc",
            "Chassis":[
                {"Id":"Board","SerialNumber":"WRONG","Assemblies":[{"MemberId":"0","Name":"Tray FRU","SerialNumber":"WRONG"}]},
                {"Id":"Tray_0","SerialNumber":" ","Assemblies":[{"MemberId":"0","Name":"Board FRU","SerialNumber":"BOARD"}]},
                {"Id":"Tray_1","SerialNumber":"CHASSIS-1","Assemblies":[{"MemberId":"0","Name":"Tray FRU","SerialNumber":"FRU-1"}]},
                {"Id":"Tray_2","SerialNumber":"CHASSIS-2","Assemblies":[{"MemberId":"0","Name":"Tray FRU","SerialNumber":"FRU-2"}]}
            ]
        })).unwrap();
        value_scenarios!(run = |(source, expected): (Value, &str)| {
            let rules = serde_json::from_value(json!([{"id":"site", "source":source}])).unwrap();
            report.matches_expected_serial_number(expected, &rules)
        };
            "first matching chassis with a serial" {
                (json!({"kind":"chassis","chassis_id":{"prefix":"tray_"}}), "CHASSIS-1") => true,
            }
            "first matching chassis with a usable FRU" {
                (json!({"kind":"chassis_assembly","chassis_id":{"contains":"RAY_"},"name":{"exact":"Tray FRU"}}), "FRU-1") => true,
            }
        );
    }

    #[test]
    fn an_unidentified_assembly_with_a_known_model_is_not_a_host_serial_source() {
        let report = report(
            "Chassis_0",
            json!([
                {"MemberId":"2","Model":"Titania-Bianca Compute Tray PDB","SerialNumber":"PRODUCT"},
                {"MemberId":"0","Name":"PDB Chassis FRU Assembly0","Model":"Other","SerialNumber":"OTHER"}
            ]),
        );
        let rules = HostSerialRules::default();
        assert!(!report.matches_expected_serial_number("PRODUCT", &rules));
        assert!(!report.matches_expected_serial_number("OTHER", &rules));
        assert!(report.matches_expected_serial_number("BOARD", &rules));
    }

    #[test]
    fn refresh_preserves_system_serial_sku_and_tray_matches_without_a_site_override() {
        let legacy: EndpointExplorationReport = serde_json::from_value(json!({
            "EndpointType":"Bmc",
            "Systems":[{"Id":"System","SerialNumber":"SYSTEM","Sku":"SKU"}],
            "Chassis":[{"Id":"Chassis_0","SerialNumber":"TRAY"}]
        }))
        .unwrap();
        let fresh = report(
            "Chassis_0",
            json!([
                {"MemberId":"0","Name":"PDB Chassis FRU Assembly0","Model":"GB200 NVL","SerialNumber":"TRAY"}
            ]),
        );
        let missing_source = serde_json::from_value(json!([{
            "id":"site-other-tray","source":{"kind":"chassis","chassis_id":{"exact":"OtherTray"}}
        }]))
        .unwrap();
        for rules in [HostSerialRules::default(), missing_source] {
            for report in [&legacy, &fresh] {
                value_scenarios!(run = |expected: &str| report.matches_expected_serial_number(expected, &rules);
                    "identifiers accepted before and after re-exploration" {
                        "SYSTEM" => true, "SKU" => true, "TRAY" => true, "BOARD" => false, "OTHER" => false,
                    }
                );
            }
        }
    }

    #[test]
    fn chassis_rule_selects_the_raw_serial_only_for_hosts() {
        let rules = serde_json::from_value(json!([{
            "id":"site","source":{"kind":"chassis","chassis_id":{"exact":"Chassis_0"}}
        }]))
        .unwrap();
        let mut report = report(
            "Chassis_0",
            json!([
                {"MemberId":"0","Name":"PDB Chassis FRU Assembly0","Model":"GB200 NVL","SerialNumber":"TRAY"}
            ]),
        );
        assert!(report.matches_expected_serial_number("BOARD", &rules));
        assert!(!report.matches_expected_serial_number("TRAY", &rules));
        assert!(!report.matches_expected_serial_number("SYSTEM", &rules));
        assert!(!report.matches_expected_serial_number("SKU", &rules));
        report.chassis.push(Chassis {
            id: "MGX_NVSwitch_0".into(),
            ..Default::default()
        });
        assert!(report.matches_expected_serial_number("TRAY", &rules));
    }
}
