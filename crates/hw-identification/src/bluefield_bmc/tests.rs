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

use super::{
    BlueFieldGeneration, BlueFieldModelRule, ModelSource, identify_generation,
    identify_generation_with_rules,
};
use crate::pattern::exact;

#[test]
fn identical_names_match_only_their_declared_redfish_source() {
    const RULES: &[BlueFieldModelRule] = &[
        BlueFieldModelRule {
            sources: &[ModelSource::ServiceRootProduct],
            model_names: &[exact("shared name")],
            generation: BlueFieldGeneration::BlueField3,
        },
        BlueFieldModelRule {
            sources: &[ModelSource::ChassisModel],
            model_names: &[exact("shared name")],
            generation: BlueFieldGeneration::BlueField4,
        },
    ];
    for (root_product, chassis_model, expected) in [
        (
            Some("shared name"),
            None,
            Some(BlueFieldGeneration::BlueField3),
        ),
        (
            None,
            Some("shared name"),
            Some(BlueFieldGeneration::BlueField4),
        ),
        (Some("shared name"), Some("shared name"), None),
    ] {
        assert_eq!(
            identify_generation_with_rules(
                RULES,
                root_product,
                chassis_model.map(|model| ("Card1", model))
            ),
            expected,
            "ServiceRoot.Product={root_product:?}, Card1.Model={chassis_model:?}"
        );
    }
}

#[test]
fn generation_from_card_bmc_inventory() {
    for (product, chassis_id, model, expected) in [
        // The mezzanine is the BMC component on older BF3 firmware.
        (
            "Nvidia-BMCMezz",
            "Card1",
            "Bluefield 3 SmartNIC Main Card",
            Some(BlueFieldGeneration::BlueField3),
        ),
        (
            "B4240",
            "Bluefield_BMC",
            "B4240",
            Some(BlueFieldGeneration::BlueField4),
        ),
        (
            "B4240V",
            "Bluefield_BMC",
            "B4240V",
            Some(BlueFieldGeneration::BlueField4),
        ),
        (
            "BlueField-4",
            "BlueField_BMC_0",
            "BlueField-4",
            Some(BlueFieldGeneration::BlueField4),
        ),
        (
            "B4240V",
            "Card1",
            "N/A",
            Some(BlueFieldGeneration::BlueField4),
        ),
        ("B4240V", "Bluefield_BMC", "BlueField-3 DPU", None),
        ("", "OtherChassis", "B4240", None),
        ("B4240V-other", "Card1", " ", None),
    ] {
        assert_eq!(
            identify_generation(Some(product), [(chassis_id, model)]),
            expected,
            "ServiceRoot.Product={product:?}, {chassis_id}.Model={model:?}"
        );
    }
}
