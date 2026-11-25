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

//! Built-in rules for identifying DPU products in host inventory.

use std::borrow::Cow::{self, Borrowed};

use super::DpuProduct::{
    BlueField2Dpu, BlueField3Dpu, BlueField3SuperNic, BlueField3SuperNic9D3B4, BlueField4Dpu,
    UnspecifiedBlueField,
};
use super::{HostBlueFieldRule, InventorySource};
use crate::hw::{
    lenovo_thinksystem_sr650_v4, lenovo_thinksystem_sr675_v3, nvidia_bluefield, nvidia_bluefield_2,
    nvidia_bluefield_3, nvidia_bluefield_4,
};

const CARD_INVENTORY: Cow<'static, [InventorySource]> = Borrowed(&[
    InventorySource::SystemPcieDevice,
    InventorySource::Chassis,
    InventorySource::ChassisNetworkAdapter,
    InventorySource::DpuProductChassis,
]);

/// Built-in host identification rules in match order, with the general BlueField match last.
pub(super) static DEFAULT_HOST_BLUEFIELD_RULES: &[HostBlueFieldRule] = &[
    HostBlueFieldRule {
        id: Borrowed("nvidia-bluefield2-dpu"),
        sources: CARD_INVENTORY,
        part_numbers: Borrowed(nvidia_bluefield_2::DPU_PART_NUMBERS),
        product: BlueField2Dpu,
    },
    HostBlueFieldRule {
        id: Borrowed("nvidia-bluefield3-dpu"),
        sources: CARD_INVENTORY,
        part_numbers: Borrowed(nvidia_bluefield_3::DPU_PART_NUMBERS),
        product: BlueField3Dpu,
    },
    HostBlueFieldRule {
        id: Borrowed("lenovo-thinksystem-bluefield3-dpu"),
        sources: CARD_INVENTORY,
        part_numbers: Borrowed(&[
            lenovo_thinksystem_sr675_v3::BLUEFIELD3_DPU_PART_NUMBER,
            lenovo_thinksystem_sr650_v4::BLUEFIELD3_DPU_PART_NUMBER,
        ]),
        product: BlueField3Dpu,
    },
    HostBlueFieldRule {
        id: Borrowed("nvidia-bluefield3-supernic-9d3b4"),
        sources: CARD_INVENTORY,
        part_numbers: Borrowed(nvidia_bluefield_3::SUPERNIC_9D3B4_PART_NUMBERS),
        product: BlueField3SuperNic9D3B4,
    },
    HostBlueFieldRule {
        id: Borrowed("nvidia-bluefield3-supernic"),
        sources: CARD_INVENTORY,
        part_numbers: Borrowed(nvidia_bluefield_3::SUPERNIC_PART_NUMBERS),
        product: BlueField3SuperNic,
    },
    HostBlueFieldRule {
        id: Borrowed("nvidia-bluefield4-dpu"),
        sources: CARD_INVENTORY,
        part_numbers: Borrowed(nvidia_bluefield_4::DPU_PART_NUMBERS),
        product: BlueField4Dpu,
    },
    HostBlueFieldRule {
        id: Borrowed("nvidia-bluefield-unspecified"),
        sources: CARD_INVENTORY,
        part_numbers: Borrowed(nvidia_bluefield::PART_NUMBERS),
        product: UnspecifiedBlueField,
    },
];
