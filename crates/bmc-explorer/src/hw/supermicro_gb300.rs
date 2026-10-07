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

use crate::hw::BiosAttr;

/// Setup attributes exposed by the Supermicro OpenBMC firmware on GB300.
///
/// Unlike GB200, this platform does not expose `TPM`, `EmbeddedUefiShell`, or
/// `SecureBootEnable`. `SecurityDeviceSupport` is its TPM control, while the
/// PCIe option ROMs must be enabled for the host to expose the DPU boot target.
pub const EXPECTED_BIOS_ATTRS: [BiosAttr; 3] = [
    BiosAttr::new_str("SecurityDeviceSupport", "Enabled"),
    BiosAttr::new_bool("Socket0Pcie6DisableOptionROM", false),
    BiosAttr::new_bool("Socket1Pcie6DisableOptionROM", false),
];
