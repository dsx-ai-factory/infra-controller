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
pub(crate) type SlotNumber = usize;

pub(crate) struct Nic<'a> {
    pub(crate) mac_address: MacAddress,
    pub(crate) serial_number: Option<Cow<'a, str>>,
    pub(crate) manufacturer: Option<Cow<'a, str>>,
    pub(crate) model: Option<Cow<'a, str>>,
    pub(crate) description: Option<Cow<'a, str>>,
    pub(crate) part_number: Option<Cow<'a, str>>,
    pub(crate) firmware_version: Option<Cow<'a, str>>,
}

impl Nic<'_> {
    pub(crate) fn rooftop(mac: MacAddress) -> Nic<'static> {
        let serial_number = Some(format!("RT{}", mac.to_string().replace(':', "")).into());
        Nic {
            manufacturer: Some("Rooftop Technologies".into()),
            model: Some("Rooftop 10 Kilobit Ethernet Adapter".into()),
            serial_number,
            part_number: Some("31337".into()),
            description: None,
            firmware_version: None,
            mac_address: mac,
        }
    }
}
