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

use mac_address::MacAddress;

use crate::hw;

// This type describes Intel® Ethernet Network Adapter E810.
pub(crate) struct NicIntelE810 {
    pub(crate) mac_addresses: [MacAddress; 2],
}

impl NicIntelE810 {
    pub(super) fn ethernet_nics(&self) -> [hw::nic::Nic<'static>; 2] {
        // Real serial numbers are MAC address of port0 without ':'.
        let serial_number = self.mac_addresses[0].to_string().replace(":", "");
        self.mac_addresses.map(|mac| hw::nic::Nic {
            mac_address: mac,
            serial_number: Some(serial_number.clone().into()),
            manufacturer: None,
            model: None,
            description: None,
            part_number: Some("K91258-010".into()),
            firmware_version: None,
        })
    }
}
