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

// tests/command_builder_tests.rs
// Tests for CommandBuilder functionality returning CommandSpec objects

use std::path::Path;

use carbide_test_support::Outcome::*;
use carbide_test_support::{scenarios, value_scenarios};
use libmlx::runner::command_builder::{CommandBuilder, CommandSpec};
use libmlx::runner::exec_options::ExecOptions;

use super::common;

// A CommandBuilder over `device` with the given options -- the struct-literal dance
// every test below would otherwise repeat.
fn builder<'a>(options: &'a ExecOptions, device: &'a str) -> CommandBuilder<'a> {
    CommandBuilder { device, options }
}

#[test]
fn test_build_query_command_spec_empty_variables() {
    let options = ExecOptions::default();
    let builder = builder(&options, "02:00.0");

    let temp_file = Path::new("/tmp/test_empty.json");
    let variables: Vec<String> = vec![];

    let command_spec = builder.build_query_command(&variables, temp_file).unwrap();

    assert_eq!(command_spec.program, "mlxconfig");
    assert!(command_spec.args.contains(&"-d".to_string()));
    assert!(command_spec.args.contains(&"02:00.0".to_string()));
    assert!(command_spec.args.contains(&"q".to_string()));

    // Should not contain any variable names
    assert!(!command_spec.args.contains(&"SRIOV_EN".to_string()));
}

#[test]
fn test_build_query_command_spec_many_variables() {
    let options = ExecOptions::default();
    let builder = builder(&options, "01:00.0");

    let temp_file = Path::new("/tmp/test_many.json");
    let variables = vec![
        "VAR1".to_string(),
        "VAR2".to_string(),
        "VAR3".to_string(),
        "ARRAY_VAR[0]".to_string(),
        "ARRAY_VAR[1]".to_string(),
    ];

    let command_spec = builder.build_query_command(&variables, temp_file).unwrap();

    for var in &variables {
        assert!(command_spec.args.contains(var));
    }
}

#[test]
fn test_build_set_command_spec_empty_assignments() {
    let options = ExecOptions::default();
    let builder = builder(&options, "01:00.0");

    let assignments: Vec<String> = vec![];

    let command_spec = builder.build_set_command(&assignments).unwrap();

    assert_eq!(command_spec.program, "mlxconfig");
    assert!(command_spec.args.contains(&"-d".to_string()));
    assert!(command_spec.args.contains(&"01:00.0".to_string()));
    assert!(command_spec.args.contains(&"--yes".to_string()));
    assert!(command_spec.args.contains(&"set".to_string()));
}

// CommandSpec's Display joins the program and its args with spaces; an arg-less
// spec is just the program.
#[test]
fn command_spec_displays_program_then_args() {
    value_scenarios!(
        run = |spec| format!("{spec}");
        "program with args" {
            CommandSpec::new("mlxconfig")
            .arg("-d")
            .arg("01:00.0")
            .arg("q")
            .arg("SRIOV_EN") => "mlxconfig -d 01:00.0 q SRIOV_EN".to_string(),
        }

        "program with no args" {
            CommandSpec::new("mlxconfig") => "mlxconfig".to_string(),
        }
    );
}

#[test]
fn test_command_spec_builder_pattern() {
    let spec = CommandSpec::new("mlxconfig")
        .arg("-d")
        .arg("01:00.0")
        .args(["-e", "-j", "/tmp/test.json"])
        .arg("q")
        .args(vec!["VAR1", "VAR2"]);

    assert_eq!(spec.program, "mlxconfig");
    assert_eq!(spec.args.len(), 8);
    assert_eq!(spec.args[0], "-d");
    assert_eq!(spec.args[1], "01:00.0");
    assert_eq!(spec.args[6], "VAR1");
    assert_eq!(spec.args[7], "VAR2");
}

// build_set_assignments turns each MlxConfigValue into one or more `VAR=value`
// (or `VAR[index]=value`) strings, in input order, skipping the None slots of a
// sparse array. The assignments come back in a deterministic order, so each row
// pins the exact Vec<String>. Scalars, every array variant (dense and sparse),
// binary hex encoding, multiple variables, and the empty case all fold here; the
// per-row config values are pre-built from the shared test registry.
#[test]
fn build_set_assignments_formats_each_value() {
    let registry = common::create_test_registry();
    // Pull a registry variable by name; each row then `.with(...)`s a concrete value.
    let var = |name: &str| registry.get_variable(name).unwrap();

    let options = ExecOptions::default();
    let builder = builder(&options, "01:00.0");

    scenarios!(
        run = |config_values| builder.build_set_assignments(&config_values).map_err(drop);
        "boolean scalar" {
            vec![var("SRIOV_EN").with(true).unwrap()] => Yields(vec!["SRIOV_EN=true".to_string()]),
        }

        "integer scalar" {
            vec![var("NUM_OF_VFS").with(32i64).unwrap()] => Yields(vec!["NUM_OF_VFS=32".to_string()]),
        }

        "enum scalar" {
            vec![var("POWER_MODE").with("HIGH").unwrap()] => Yields(vec!["POWER_MODE=HIGH".to_string()]),
        }

        "preset scalar" {
            vec![var("PERFORMANCE_PRESET").with(7u8).unwrap()] => Yields(vec!["PERFORMANCE_PRESET=7".to_string()]),
        }

        "binary scalar is hex-encoded" {
            vec![
                var("DEVICE_UUID")
                    .with(vec![0x1au8, 0x2bu8, 0x3cu8, 0x4du8])
                    .unwrap(),
            ] => Yields(vec!["DEVICE_UUID=0x1a2b3c4d".to_string()]),
        }

        "dense boolean array sets every index" {
            vec![
                var("GPIO_ENABLED")
                    .with(vec![true, false, true, false])
                    .unwrap(),
            ] => Yields(vec![
                "GPIO_ENABLED[0]=true".to_string(),
                "GPIO_ENABLED[1]=false".to_string(),
                "GPIO_ENABLED[2]=true".to_string(),
                "GPIO_ENABLED[3]=false".to_string(),
            ]),
        }

        "sparse boolean array skips None slots" {
            vec![
                var("GPIO_ENABLED")
                    .with(vec![Some(true), None, Some(false), None])
                    .unwrap(),
            ] => Yields(vec![
                "GPIO_ENABLED[0]=true".to_string(),
                "GPIO_ENABLED[2]=false".to_string(),
            ]),
        }

        "sparse integer array skips None slots" {
            vec![
                var("THERMAL_SENSORS")
                    .with(vec![
                        Some(45i64),
                        None,
                        Some(42i64),
                        None,
                        Some(39i64),
                        None,
                    ])
                    .unwrap(),
            ] => Yields(vec![
                "THERMAL_SENSORS[0]=45".to_string(),
                "THERMAL_SENSORS[2]=42".to_string(),
                "THERMAL_SENSORS[4]=39".to_string(),
            ]),
        }

        "sparse enum array skips None slots" {
            vec![
                var("GPIO_MODES")
                    .with(vec![
                        Some("input".to_string()),
                        Some("output".to_string()),
                        None,
                        Some("bidirectional".to_string()),
                        None,
                        None,
                        None,
                        None,
                    ])
                    .unwrap(),
            ] => Yields(vec![
                "GPIO_MODES[0]=input".to_string(),
                "GPIO_MODES[1]=output".to_string(),
                "GPIO_MODES[3]=bidirectional".to_string(),
            ]),
        }

        "multiple variables, each in input order" {
            vec![
                var("SRIOV_EN").with(true).unwrap(),
                var("NUM_OF_VFS").with(16i64).unwrap(),
                var("POWER_MODE").with("HIGH").unwrap(),
            ] => Yields(vec![
                "SRIOV_EN=true".to_string(),
                "NUM_OF_VFS=16".to_string(),
                "POWER_MODE=HIGH".to_string(),
            ]),
        }

        "no values yields no assignments" {
            vec![] => Yields(vec![]),
        }
    );
}

#[test]
fn test_realistic_mlxconfig_query_spec() {
    let options = ExecOptions::default();
    let builder = builder(&options, "01:00.0");

    let variables = vec![
        "SRIOV_EN".to_string(),
        "NUM_OF_VFS".to_string(),
        "POWER_MODE".to_string(),
    ];
    let temp_file = Path::new("/tmp/output.json");

    let command_spec = builder.build_query_command(&variables, temp_file).unwrap();

    let command_str = format!("{command_spec}");
    assert!(command_str.contains("mlxconfig -d 01:00.0 -e -j /tmp/output.json q SRIOV_EN"));
    assert!(command_str.contains("NUM_OF_VFS"));
    assert!(command_str.contains("POWER_MODE"));
}

#[test]
fn test_realistic_mlxconfig_set_spec() {
    let options = ExecOptions::default();
    let builder = builder(&options, "01:00.0");

    let assignments = vec!["SRIOV_EN=true".to_string(), "NUM_OF_VFS=16".to_string()];

    let command_spec = builder.build_set_command(&assignments).unwrap();

    let command_str = format!("{command_spec}");
    assert!(command_str.contains("mlxconfig -d 01:00.0 --yes set SRIOV_EN=true NUM_OF_VFS=16"));
}
