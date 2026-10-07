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

// tests/common/mod.rs
// Shared test utilities for mlxconfig-runner tests.

use libmlx::runner::result_types::QueriedDeviceInfo;
use libmlx::variables::registry::MlxVariableRegistry;
use libmlx::variables::spec::MlxVariableSpec;
use libmlx::variables::variable::MlxConfigVariable;
use serde_json::json;

/// Creates a test registry with common variable types for testing
pub(super) fn create_test_registry() -> MlxVariableRegistry {
    let variables = vec![
        // Boolean variable
        MlxConfigVariable::builder()
            .name("SRIOV_EN")
            .description("Enable Single-Root I/O Virtualization")
            .read_only(false)
            .spec(MlxVariableSpec::builder().boolean().build())
            .build(),
        // Integer variable
        MlxConfigVariable::builder()
            .name("NUM_OF_VFS")
            .description("Number of Virtual Functions")
            .read_only(false)
            .spec(MlxVariableSpec::builder().integer().build())
            .build(),
        // Enum variable
        MlxConfigVariable::builder()
            .name("POWER_MODE")
            .description("Power management mode")
            .read_only(false)
            .spec(
                MlxVariableSpec::builder()
                    .enum_type()
                    .with_options(vec![
                        "LOW".to_string(),
                        "MEDIUM".to_string(),
                        "HIGH".to_string(),
                    ])
                    .build(),
            )
            .build(),
        // Read-only string variable
        MlxConfigVariable::builder()
            .name("DEVICE_NAME")
            .description("Hardware device name")
            .read_only(true)
            .spec(MlxVariableSpec::builder().string().build())
            .build(),
        // Preset variable
        MlxConfigVariable::builder()
            .name("PERFORMANCE_PRESET")
            .description("Performance optimization preset")
            .read_only(false)
            .spec(
                MlxVariableSpec::builder()
                    .preset()
                    .with_max_preset(10)
                    .build(),
            )
            .build(),
        // Boolean array variable
        MlxConfigVariable::builder()
            .name("GPIO_ENABLED")
            .description("GPIO pin enable status")
            .read_only(false)
            .spec(
                MlxVariableSpec::builder()
                    .boolean_array()
                    .with_size(4)
                    .build(),
            )
            .build(),
        // Integer array variable
        MlxConfigVariable::builder()
            .name("THERMAL_SENSORS")
            .description("Thermal sensor readings")
            .read_only(true)
            .spec(
                MlxVariableSpec::builder()
                    .integer_array()
                    .with_size(6)
                    .build(),
            )
            .build(),
        // Enum array variable
        MlxConfigVariable::builder()
            .name("GPIO_MODES")
            .description("GPIO pin mode configuration")
            .read_only(false)
            .spec(
                MlxVariableSpec::builder()
                    .enum_array()
                    .with_options(vec![
                        "input".to_string(),
                        "output".to_string(),
                        "bidirectional".to_string(),
                    ])
                    .with_size(8)
                    .build(),
            )
            .build(),
        // Binary variable
        MlxConfigVariable::builder()
            .name("DEVICE_UUID")
            .description("Device unique identifier")
            .read_only(true)
            .spec(MlxVariableSpec::builder().binary().build())
            .build(),
    ];

    MlxVariableRegistry::new("Test Registry").variables(variables)
}

/// Creates a simple registry with minimal variables for focused testing
pub(super) fn create_minimal_test_registry() -> MlxVariableRegistry {
    let variables = vec![
        MlxConfigVariable::builder()
            .name("TEST_BOOL")
            .description("Test boolean variable")
            .read_only(false)
            .spec(MlxVariableSpec::builder().boolean().build())
            .build(),
        MlxConfigVariable::builder()
            .name("TEST_INT")
            .description("Test integer variable")
            .read_only(false)
            .spec(MlxVariableSpec::builder().integer().build())
            .build(),
    ];

    MlxVariableRegistry::new("Minimal Test Registry".to_string()).variables(variables)
}

/// Creates sample mlxconfig JSON response for testing
pub(super) fn create_sample_json_response(device: &str) -> serde_json::Value {
    json!({
        "Device #1": {
            "description": "Test BlueField-3 Device",
            "device": device,
            "device_type": "BlueField3",
            "name": "900-9D3D4-00EN-HA0_Ax",
            "tlv_configuration": {
                "SRIOV_EN": {
                    "current_value": "True(1)",
                    "default_value": "False(0)",
                    "modified": true,
                    "next_value": "True(1)",
                    "read_only": false
                },
                "NUM_OF_VFS": {
                    "current_value": 16,
                    "default_value": 8,
                    "modified": true,
                    "next_value": 16,
                    "read_only": false
                },
                "POWER_MODE": {
                    "current_value": "HIGH(2)",
                    "default_value": "MEDIUM(1)",
                    "modified": true,
                    "next_value": "HIGH(2)",
                    "read_only": false
                },
                "DEVICE_NAME": {
                    "current_value": "test-device",
                    "default_value": "test-device",
                    "modified": false,
                    "next_value": "test-device",
                    "read_only": true
                },
                "PERFORMANCE_PRESET": {
                    "current_value": 5,
                    "default_value": 0,
                    "modified": true,
                    "next_value": 5,
                    "read_only": false
                }
            }
        }
    })
}

#[allow(dead_code)]
/// Creates test device info
pub(super) fn create_test_device_info() -> QueriedDeviceInfo {
    QueriedDeviceInfo::new()
        .with_device_id("01:00.0")
        .with_device_type("BlueField3")
        .with_part_number("900-9D3D4-00EN-HA0")
        .with_description("Some device")
}
