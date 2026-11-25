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

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    host_serial_rules: HostSerialRules,
}

#[test]
fn toml_round_trip_preserves_site_precedence_and_hardware_class() {
    let config: Config = toml::from_str(
        r#"
        [[host_serial_rules]]
        id = "site-tray"
        hardware_class = { prefix = "ACME_" }
        source = { kind = "system_sku" }

        [[host_serial_rules]]
        id = "all-hosts"
        source = { kind = "system_serial" }
    "#,
    )
    .unwrap();
    let saved: Config = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert_eq!(saved.host_serial_rules, config.host_serial_rules);

    let defaults = HostSerialRules::default();
    assert!(defaults.uses_only_defaults());
    assert!(!saved.host_serial_rules.uses_only_defaults());
    let sku = HostSerialSource::SystemSku {};
    let serial = HostSerialSource::SystemSerial {};
    for (class, site_sources) in [
        (Some("acme_tray"), &[&sku, &serial][..]),
        (Some("other"), &[&serial][..]),
        (None, &[&serial][..]),
    ] {
        assert_eq!(
            saved
                .host_serial_rules
                .site_sources(class)
                .collect::<Vec<_>>(),
            site_sources,
            "{class:?}"
        );
    }
}

#[test]
fn default_sources_select_the_chassis_fru_by_its_exact_name_and_model() {
    use crate::pattern::exact;

    for (name, model) in [
        ("PDB Chassis FRU Assembly0", "GB200 NVL"),
        (
            "PDB Chassis FRU Assembly0",
            "Titania-Bianca Compute Tray PDB",
        ),
        ("PDB Chassis FRU Assembly0", "P4107"),
        ("PDB_0 Chassis FRU Assembly0", "P4107"),
    ] {
        let expected = HostSerialSource::ChassisAssembly {
            chassis_id: exact("Chassis_0"),
            name: Some(exact(name)),
            model: Some(exact(model)),
            member_id: None,
        };
        assert!(
            HostSerialRules::built_in_sources(None).any(|source| source == &expected),
            "{name} / {model}"
        );
    }
}

#[test]
fn assembly_collection_includes_site_and_default_chassis_before_the_class_is_known() {
    let config: Config = toml::from_str(r#"
        [[host_serial_rules]]
        id = "site-tray"
        hardware_class = { exact = "acme_tray" }
        source = { kind = "chassis_assembly", chassis_id = { prefix = "Tray_" }, member_id = { exact = "0" } }

        [[host_serial_rules]]
        id = "board-serial"
        source = { kind = "chassis", chassis_id = { contains = "Board" } }
    "#).unwrap();
    let defaults = HostSerialRules::default();
    for (id, builtin, with_site) in [
        ("chassis_0", true, true),
        ("Chassis_01", false, false),
        ("TRAY_2", false, true),
        ("MainBoard", false, false),
    ] {
        assert_eq!(defaults.needs_chassis_assembly(id), builtin, "{id}");
        assert_eq!(
            config.host_serial_rules.needs_chassis_assembly(id),
            with_site,
            "{id}"
        );
    }
}

#[test]
fn invalid_site_rule_ids_identify_the_conflict_or_index() {
    let rule = |id: &'static str, source| HostSerialRule {
        id: id.into(),
        hardware_class: None,
        source,
    };
    for (id, message) in [
        ("site", r#"duplicate host serial rule ID "site""#),
        (
            "gb200-pdb-chassis-fru",
            r#"duplicate host serial rule ID "gb200-pdb-chassis-fru""#,
        ),
        ("", "at index 1:"),
        (" padded ", "at index 1:"),
    ] {
        let error = HostSerialRules::try_from(vec![
            rule("site", HostSerialSource::SystemSerial {}),
            rule(id, HostSerialSource::SystemSku {}),
        ])
        .unwrap_err();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn invalid_serial_sources_are_rejected_when_deserializing_toml() {
    for (fields, message) in [
        (
            r#"hardware_class = { prefix = " " }
source = { kind = "system_serial" }"#,
            "blank hardware class pattern",
        ),
        (
            r#"source = { kind = "chassis", chassis_id = { contains = " " } }"#,
            "blank chassis_id pattern",
        ),
        (
            r#"source = { kind = "chassis_assembly", chassis_id = { exact = "Tray" } }"#,
            "at least one assembly selector",
        ),
        (
            r#"source = { kind = "chassis_assembly", chassis_id = { exact = "Tray" }, name = { exact = "FRU" }, model = { prefix = " " } }"#,
            "blank assembly selector",
        ),
        (
            r#"source = { kind = "chassis", chassis_id = { exact = "Tray", prefix = "T" } }"#,
            "exactly one operator",
        ),
        (r#"source = { kind = "assemblyy" }"#, "unknown variant"),
    ] {
        let input = format!("[[host_serial_rules]]\nid = \"site-tray\"\n{fields}");
        let error = toml::from_str::<Config>(&input).unwrap_err();
        assert!(error.to_string().contains(message), "{fields}: {error}");
    }
}
