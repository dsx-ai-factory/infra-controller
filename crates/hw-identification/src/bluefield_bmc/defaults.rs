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

//! Built-in rules for identifying BlueField generations by model name.

use super::{BlueFieldGeneration, BlueFieldModelRule, ModelSource};
use crate::hw::{nvidia_bluefield_2, nvidia_bluefield_3, nvidia_bluefield_4};

pub(super) static DEFAULT_BLUEFIELD_MODEL_RULES: &[BlueFieldModelRule] = &[
    BlueFieldModelRule {
        sources: &[ModelSource::ServiceRootProduct, ModelSource::ChassisModel],
        model_names: nvidia_bluefield_2::MODEL_NAMES,
        generation: BlueFieldGeneration::BlueField2,
    },
    BlueFieldModelRule {
        sources: &[ModelSource::ServiceRootProduct, ModelSource::ChassisModel],
        model_names: nvidia_bluefield_3::MODEL_NAMES,
        generation: BlueFieldGeneration::BlueField3,
    },
    BlueFieldModelRule {
        sources: &[ModelSource::ServiceRootProduct, ModelSource::ChassisModel],
        model_names: nvidia_bluefield_4::BMC_MODEL_NAMES,
        generation: BlueFieldGeneration::BlueField4,
    },
];
