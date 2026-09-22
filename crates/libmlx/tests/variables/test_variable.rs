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

use libmlx::variables::spec::MlxVariableSpec;
use libmlx::variables::variable::MlxConfigVariable;

#[test]
fn test_mlx_config_variable_builder_basic() {
    let variable = MlxConfigVariable::builder()
        .name("cpu_frequency")
        .description("CPU frequency in MHz")
        .read_only(false)
        .spec(MlxVariableSpec::Integer)
        .build();

    assert_eq!(variable.name, "cpu_frequency");
    assert_eq!(variable.description, "CPU frequency in MHz");
    assert!(!variable.read_only);
    assert!(matches!(variable.spec, MlxVariableSpec::Integer));
}

#[test]
fn test_mlx_config_variable_builder_read_only() {
    let variable = MlxConfigVariable::builder()
        .name("firmware_version")
        .description("Current firmware version")
        .read_only(true)
        .spec(MlxVariableSpec::String)
        .build();

    assert_eq!(variable.name, "firmware_version");
    assert_eq!(variable.description, "Current firmware version");
    assert!(variable.read_only);
    assert!(matches!(variable.spec, MlxVariableSpec::String));
}

#[test]
#[should_panic(expected = "name is required")]
fn test_mlx_config_variable_builder_missing_name() {
    MlxConfigVariable::builder()
        .description("Test description")
        .spec(MlxVariableSpec::Boolean)
        .build();
}

#[test]
#[should_panic(expected = "description is required")]
fn test_mlx_config_variable_builder_missing_description() {
    MlxConfigVariable::builder()
        .name("test_var")
        .spec(MlxVariableSpec::Boolean)
        .build();
}

#[test]
#[should_panic(expected = "spec is required")]
fn test_mlx_config_variable_builder_missing_spec() {
    MlxConfigVariable::builder()
        .name("test_var")
        .description("Test description")
        .build();
}

#[test]
fn test_mlx_config_variable_builder_default_read_only() {
    let variable = MlxConfigVariable::builder()
        .name("test_var")
        .description("Test description")
        .spec(MlxVariableSpec::Boolean)
        .build();

    // Default should be false (writable)
    assert!(!variable.read_only);
}
