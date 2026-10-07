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

use std::borrow::Cow;

use mac_address::MacAddress;

use crate::hw;

// This type describes NVIDIA ConnectX-7A Dual Port NIC.
pub(crate) struct NicNvidiaCx7A<'a> {
    pub(crate) serial_number: Cow<'a, str>,
    pub(crate) mac_addresses: [MacAddress; 2],
}

impl NicNvidiaCx7A<'_> {
    pub(super) fn ethernet_nics(&self) -> [hw::nic::Nic<'_>; 2] {
        self.mac_addresses.map(|mac| hw::nic::Nic {
            mac_address: mac,
            serial_number: Some(self.serial_number.clone()),
            manufacturer: Some("MLNX".into()),
            model: Some("MCX755206AS-692          ".into()),
            description: None,
            part_number: Some("CX755206A      ".into()),
            firmware_version: None,
        })
    }
}

// This type describes NVIDIA ConnectX-7B 4x port NIC Ethernet/IB.
pub(crate) struct NicNvidiaCx7B<'a> {
    pub(crate) serial_number: Cow<'a, str>,
    pub(crate) mac_addresses: [MacAddress; 4],
}

impl NicNvidiaCx7B<'_> {
    pub(super) fn ib_nics(&self) -> [hw::nic::Nic<'_>; 4] {
        self.mac_addresses.map(|mac_address| hw::nic::Nic {
            mac_address,
            serial_number: Some(self.serial_number.clone()),
            manufacturer: Some("MLNX".into()),
            model: Some("CX750500B".into()),
            description: None,
            part_number: Some("MCX750500B-692".into()),
            firmware_version: None,
        })
    }
}
