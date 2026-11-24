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

//! NVIDIA BlueField-3.

use crate::pattern::{Pattern, prefix};

/// NVIDIA part number for the 00CV-AA0 variant of the BlueField-3 9D3B6 DPU.
pub const DPU_9D3B6_00CV_AA0: &str = "900-9D3B6-00CV-AA0";

/// NVIDIA part number for the 00EN-EA0 variant of the BlueField-3 9D3B4 SuperNIC.
pub const SUPERNIC_9D3B4_00EN_EA0: &str = "900-9D3B4-00EN-EA0";

/// Part-number prefix for BlueField-3 DPUs.
pub const DPU_PART_NUMBERS: &[Pattern] = &[prefix("900-9D3B6")];

/// Part-number prefix for BlueField-3 9D3B4 SuperNICs.
pub const SUPERNIC_9D3B4_PART_NUMBERS: &[Pattern] = &[prefix("900-9D3B4")];

/// Part-number prefix for BlueField-3 9D3D4 SuperNICs.
pub const SUPERNIC_PART_NUMBERS: &[Pattern] = &[prefix("900-9D3D4")];
