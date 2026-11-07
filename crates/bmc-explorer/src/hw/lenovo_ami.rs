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

pub const EXPECTED_BIOS_ATTRS: [BiosAttr; 10] = [
    BiosAttr::new_str("VMXEN", "Enable"), // VMX (Intel Virtualization)
    BiosAttr::new_str("PCIS007", "Enabled"), // SR-IOV Support
    BiosAttr::new_int("LEM0001", 3),      // PXE retry count (remove on future FW update)
    BiosAttr::new_str("NWSK000", "Enabled"), // Network Stack
    BiosAttr::new_str("NWSK001", "Disabled"), // IPv4 PXE Support
    BiosAttr::new_str("NWSK006", "Enabled"), // IPv4 HTTP Support
    BiosAttr::new_str("NWSK002", "Disabled"), // IPv6 PXE Support
    BiosAttr::new_str("NWSK007", "Disabled"), // IPv6 HTTP Support
    BiosAttr::new_str("FBO001", "UEFI"),  // Boot Mode Select
    BiosAttr::new_str("EndlessBoot", "Enabled"), // Infinite Boot
];
