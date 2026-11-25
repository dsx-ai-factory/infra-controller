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

//! NVIDIA BlueField identification shared across generations.

use crate::pattern::{Pattern, contains};

/// Matches part numbers containing the BlueField family name.
pub const PART_NUMBERS: &[Pattern] = &[contains("BlueField")];

/// Case-sensitive ComputerSystem IDs published by BlueField BMC firmware.
/// Newer BF4 firmware uses the suffixed ID (infra-controller #3117).
pub const SYSTEM_IDS: &[&str] = &["Bluefield", "BlueField_0"];

/// Chassis IDs that carry card identity in BlueField Redfish inventory.
/// Firmware exposes the card on Card1 or the integrated BMC chassis (#3117, #8647).
pub const PRODUCT_CHASSIS_IDS: &[&str] = &["Card1", "Bluefield_BMC", "BlueField_BMC_0"];
