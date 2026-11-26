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

use super::*;
use crate::hw::nvidia_bluefield_3::{DPU_9D3B6_00CV_AA0 as BLUEFIELD, SUPERNIC_9D3B4_00EN_EA0};

#[test]
fn site_rules_extend_defaults_only_at_their_declared_sources() {
    use InventorySource::{ChassisNetworkAdapter, SystemPcieDevice};

    #[derive(Deserialize, Serialize)]
    struct Config {
        host_bluefield_rules: HostBlueFieldRules,
    }
    let config: Config = toml::from_str(
        r#"
        [[host_bluefield_rules]]
        id = "site-oem-bf4"
        sources = ["chassis_network_adapter"]
        part_numbers = [{exact = "OEM-X"}, {prefix = "900-9D3B6"}]
        product = "bluefield4_dpu"

        [[host_bluefield_rules]]
        id = "later-oem-rule"
        sources = ["chassis_network_adapter"]
        part_numbers = [{exact = "OEM-X"}]
        product = "bluefield2_dpu"
    "#,
    )
    .unwrap();
    let encoded = toml::to_string(&config).unwrap();
    let saved: Config = toml::from_str(&encoded).unwrap();
    assert_eq!(saved.host_bluefield_rules, config.host_bluefield_rules);

    let site_match = Some(("site-oem-bf4", DpuProduct::BlueField4Dpu));
    let builtin_match = Some(("nvidia-bluefield3-dpu", DpuProduct::BlueField3Dpu));
    for (source, part_number, expected) in [
        (ChassisNetworkAdapter, " oem-x ", site_match),
        (ChassisNetworkAdapter, "OEM-X-more", None),
        (SystemPcieDevice, "OEM-X", None),
        (ChassisNetworkAdapter, BLUEFIELD, site_match),
        (SystemPcieDevice, BLUEFIELD, builtin_match),
    ] {
        let matched = config
            .host_bluefield_rules
            .matching_rule(source, part_number);
        assert_eq!(
            matched.map(|rule| (rule.id.as_ref(), rule.product)),
            expected,
            "{source:?} {part_number:?}"
        );
    }
}

#[test]
fn first_matching_site_rule_wins_at_its_source() {
    use DpuProduct::{BlueField3Dpu, BlueField3SuperNic, BlueField4Dpu};
    use InventorySource::{Chassis, SystemPcieDevice};

    use crate::pattern::{exact, prefix};

    let site_rule = |id: &'static str, pattern, product| HostBlueFieldRule {
        id: id.into(),
        sources: vec![Chassis].into(),
        part_numbers: vec![pattern].into(),
        product,
    };
    let sku = site_rule("site-sku", exact(BLUEFIELD), BlueField4Dpu);
    let family = site_rule("site-family", prefix("900-9D3B6"), BlueField3SuperNic);

    for (first, second) in [(&sku, &family), (&family, &sku)] {
        let rules = HostBlueFieldRules::try_from(vec![first.clone(), second.clone()]).unwrap();
        assert_eq!(rules.matching_rule(Chassis, BLUEFIELD), Some(first));
        let builtin = rules.matching_rule(SystemPcieDevice, BLUEFIELD).unwrap();
        assert_eq!(
            (builtin.id.as_ref(), builtin.product),
            ("nvidia-bluefield3-dpu", BlueField3Dpu)
        );
    }
}

#[test]
fn invalid_site_rules_are_rejected_at_load_time() {
    #[derive(Deserialize)]
    struct Config {
        #[serde(rename = "rules")]
        _rules: HostBlueFieldRules,
    }
    for rule in [
        r#"{sources=["chassis"], part_numbers=[{exact="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="", sources=["chassis"], part_numbers=[{exact="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="nvidia-bluefield4-dpu", sources=["chassis"], part_numbers=[{exact="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="site-test", sources=[], part_numbers=[{exact="PN"}], product="bluefield3_dpu"}"#,
        r#"{id="site-test", sources=["chassis"], part_numbers=[], product="bluefield3_dpu"}"#,
        r#"{id="site-test", sources=["chassis"], part_numbers=[{}], product="bluefield3_dpu"}"#,
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
    let site_rule = |id: &'static str| HostBlueFieldRule {
        id: id.into(),
        ..DEFAULT_HOST_BLUEFIELD_RULES[0].clone()
    };
    for (rules, index) in [
        (vec![site_rule("")], 0),
        (vec![site_rule("site-card"), site_rule(" padded ")], 1),
    ] {
        let error = HostBlueFieldRules::try_from(rules).unwrap_err();
        assert!(error.contains(&format!("at index {index}:")), "{error}");
    }
}

#[test]
fn repeated_site_rule_ids_are_rejected_even_for_different_patterns() {
    #[derive(Debug, Deserialize)]
    struct Config {
        #[serde(rename = "rules")]
        _rules: HostBlueFieldRules,
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
    let rules = HostBlueFieldRules::default();
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
            rules.classify_part_number(InventorySource::SystemPcieDevice, part),
            expected,
            "{part:?}"
        );
    }
}
