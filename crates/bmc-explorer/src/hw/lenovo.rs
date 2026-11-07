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

pub const EXPECTED_BIOS_ATTRS: [BiosAttr; 14] = [
    // Serial console enabled:
    BiosAttr::new_str("DevicesandIOPorts_COMPort1", "Enabled"),
    BiosAttr::new_str("DevicesandIOPorts_ConsoleRedirection", "Enabled"),
    BiosAttr::new_str("DevicesandIOPorts_SerialPortSharing", "Enabled"),
    BiosAttr::new_str("DevicesandIOPorts_SPRedirection", "Enabled"),
    BiosAttr::new_str("DevicesandIOPorts_COMPortActiveAfterBoot", "Enabled"),
    BiosAttr::new_str("DevicesandIOPorts_SerialPortAccessMode", "Shared"),
    // Virtualization enabled:
    BiosAttr::new_str("Processors_IntelVirtualizationTechnology", "Enabled"), // Intel
    BiosAttr::new_str("Processors_SVMMode", "Enabled"),                       // AMD
    // UEFI:
    BiosAttr::new_str("BootModes_SystemBootMode", "UEFIMode"),
    BiosAttr::new_str("NetworkStackSettings_IPv4HTTPSupport", "Enabled"),
    BiosAttr::new_str("NetworkStackSettings_IPv4PXESupport", "Disabled"),
    BiosAttr::new_str("NetworkStackSettings_IPv6PXESupport", "Disabled"),
    BiosAttr::new_str("BootModes_InfiniteBootRetry", "Enabled"),
    BiosAttr::new_str("BootModes_PreventOSChangesToBootOrder", "Enabled"),
];
