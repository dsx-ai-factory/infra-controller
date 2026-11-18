/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
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

//! Giga Computing (GIGABYTE) hosts with an AMI MegaRAC BMC, e.g. R263-ZG0.
//!
//! The expected values mirror what libredfish's `giga_computing_ami` provider
//! writes during `machine_setup` and `setup_serial_console`.
//!
//! # Warning
//!
//! Lockdown on this platform cannot be verified end to end. The IPMI KCS
//! interface is neither configurable nor readable over Redfish on this BMC
//! (MegaRAC on AST2600), so `lockdown_status` only covers the Redfish host
//! interface. `Enabled` means "every path visible over Redfish is closed", not
//! "the host cannot reach the BMC". A process on the host with KCS access can
//! still talk to the BMC without credentials, including fetching bootstrap
//! credentials with an administrator role. Use this platform only in trusted
//! environments, or disable KCS out of band at the BMC firmware level (for
//! example with `ipmitool channel setaccess`) before handing the host to a
//! tenant.

use crate::hw::BiosAttr;

pub const EXPECTED_BIOS_ATTRS: [BiosAttr; 16] = [
    // Serial console redirected to the BMC SOL port:
    BiosAttr::new_str("TER001", "Enabled"), // Console Redirection
    BiosAttr::new_str("TER010", "Enabled"), // Console Redirection EMS
    BiosAttr::new_str("TER06B", "COM1/SOL"), // Out-of-Band Mgmt Port
    BiosAttr::new_str("TER021", "115200"),  // Bits per second
    BiosAttr::new_str("TER020", "115200"),  // Bits per second EMS
    BiosAttr::new_str("TER012", "VT100Plus"), // Terminal Type
    BiosAttr::new_str("TER011", "VT-UTF8"), // Terminal Type EMS
    BiosAttr::new_str("TER05D", "None"),    // Flow Control
    // Virtualization enabled:
    BiosAttr::new_str("CPU005", "Enabled"), // CPU Virtualization
    BiosAttr::new_str("PCIS007", "Enabled"), // SR-IOV Support
    // HTTP/PXE:
    BiosAttr::new_str("NWSK000", "Enabled"),  // Network Stack
    BiosAttr::new_str("NWSK001", "Disabled"), // IPv4 PXE Support
    BiosAttr::new_str("NWSK006", "Enabled"),  // IPv4 HTTP Support
    BiosAttr::new_str("NWSK002", "Disabled"), // IPv6 PXE Support
    BiosAttr::new_str("NWSK007", "Disabled"), // IPv6 HTTP Support
    // Infinite boot ("Endless Retry Boot", vendor-coded instead of AMI's EndlessBoot):
    BiosAttr::new_str("GBT0183", "Enabled"),
];
