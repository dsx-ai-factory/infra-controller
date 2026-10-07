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

use carbide_utils::config::as_std_duration;
use duration_str::deserialize_duration;
use serde::{Deserialize, Serialize};

/// MachineValidation related configuration
#[derive(Default, Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BomValidationConfig {
    /// Whether BOM Validation is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Allow machines that do not have a SKU assigned to bypass SKU validation
    /// When true, machines in WaitingForSkuAssignment state can proceed without a SKU
    #[serde(default)]
    pub ignore_unassigned_machines: bool,

    /// Allow machines with assigned SKUs to stay in Ready state and remain allocatable even when SKU validation fails
    /// When false (default): Standard mode - validation failures block allocation (machine enters failed state)
    /// When true: Allow allocation mode - validation still occurs and health reports are recorded, but machines do not transition
    /// into failed states (SkuVerificationFailed, SkuMissing) and can proceed to Ready/MachineValidation
    /// This does not bypass machines without an assigned SKU; use ignore_unassigned_machines for that.
    #[serde(default)]
    pub allow_allocation_on_validation_failure: bool,

    /// The interval since the last time the state machine attempted
    /// to find an existing SKU that matches the machine.
    #[serde(
        default = "BomValidationConfig::default_bom_validation_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub find_match_interval: std::time::Duration,

    /// When a SKU is assigned to a machine, but doesn't exist
    /// attempt to create a SKU for the machine.  This only
    /// applies to SKUs assigned via expected machines.
    #[serde(default)]
    pub auto_generate_missing_sku: bool,
    /// The inteveral between attempting to generate a SKU from amachine
    #[serde(
        default = "BomValidationConfig::default_bom_validation_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub auto_generate_missing_sku_interval: std::time::Duration,
}

impl BomValidationConfig {
    const fn default_bom_validation_interval() -> std::time::Duration {
        std::time::Duration::from_secs(300)
    }
}
