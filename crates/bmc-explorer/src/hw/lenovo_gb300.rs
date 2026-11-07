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

pub const EXPECTED_BIOS_ATTRS: [BiosAttr; 8] = [
    BiosAttr::new_str("PCIS007", "PCIS007Enabled"), // SR-IOV Support
    BiosAttr::new_int("LEM0001", 0),                // PXE retry count
    BiosAttr::new_str("NWSK000", "NWSK000Enabled"), // Network Stack
    BiosAttr::new_str("NWSK001", "NWSK001Disabled"), // IPv4 PXE Support
    BiosAttr::new_str("NWSK006", "NWSK006Enabled"), // IPv4 HTTP Support
    BiosAttr::new_str("NWSK002", "NWSK002Disabled"), // IPv6 PXE Support
    BiosAttr::new_str("NWSK007", "NWSK007Disabled"), // IPv6 HTTP Support
    BiosAttr::new_int("LEM0003", 50),               // Infinite Boot
];
