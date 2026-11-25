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

//! NVIDIA BlueField-4.

use crate::pattern::{Pattern, exact, prefix};

/// Part-number prefixes for the BlueField-4 B4240 and B4240V DPUs.
pub const DPU_PART_NUMBERS: &[Pattern] = &[
    prefix("900-9D4B4"), // B4240
    prefix("900-9D4A4"), // B4240V
];

/// BF4 Redfish ServiceRoot.Product and product Chassis.Model values.
/// The board names occur on earlier firmware (#3044, #3053), and BF4 26.04 also reports "BlueField-4".
pub const BMC_MODEL_NAMES: &[Pattern] = &[exact("B4240"), exact("B4240V"), exact("BlueField-4")];
