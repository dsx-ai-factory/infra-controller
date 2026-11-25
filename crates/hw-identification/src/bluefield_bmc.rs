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

//! BlueField generation identification from the card's own Redfish inventory.

mod defaults;

use defaults::DEFAULT_BLUEFIELD_MODEL_RULES;

use crate::hw::nvidia_bluefield;
use crate::pattern::Pattern;

/// Hardware generation of a BlueField card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlueFieldGeneration {
    BlueField2,
    BlueField3,
    BlueField4,
}

/// Identifies a generation when a model name matches at one of the listed sources.
struct BlueFieldModelRule {
    sources: &'static [ModelSource],
    model_names: &'static [Pattern],
    generation: BlueFieldGeneration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModelSource {
    /// The `Product` field at `/redfish/v1` on the card's own BMC.
    ServiceRootProduct,
    /// The `Model` field at `/redfish/v1/Chassis/{id}` on the card's own BMC,
    /// for one of the [product chassis IDs](nvidia_bluefield::PRODUCT_CHASSIS_IDS).
    ChassisModel,
}

/// Whether a primary Redfish ComputerSystem ID identifies the endpoint as a BlueField BMC.
pub fn is_bluefield_system_id(id: &str) -> bool {
    nvidia_bluefield::SYSTEM_IDS.contains(&id)
}

/// Whether the chassis can carry the BlueField card's product identity.
pub fn is_dpu_product_chassis_id(id: &str) -> bool {
    nvidia_bluefield::PRODUCT_CHASSIS_IDS.contains(&id)
}

/// Identifies the generation of a BlueField endpoint from ServiceRoot.Product and
/// (Chassis.Id, Chassis.Model) pairs. Unrecognized values are ignored. All
/// recognized values must agree. Returns `None` if none match or generations conflict.
pub fn identify_generation<'a>(
    root_product: Option<&str>,
    chassis_models: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<BlueFieldGeneration> {
    identify_generation_with_rules(DEFAULT_BLUEFIELD_MODEL_RULES, root_product, chassis_models)
}

fn identify_generation_with_rules<'a>(
    rules: &[BlueFieldModelRule],
    root_product: Option<&str>,
    chassis_models: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<BlueFieldGeneration> {
    let root_generation = root_product
        .and_then(|product| match_model(rules, ModelSource::ServiceRootProduct, product));
    let chassis_generations = chassis_models
        .into_iter()
        .filter(|(id, _)| is_dpu_product_chassis_id(id))
        .filter_map(|(_, model)| match_model(rules, ModelSource::ChassisModel, model));
    let mut generations = root_generation.into_iter().chain(chassis_generations);
    let generation = generations.next()?;
    generations
        .all(|other| other == generation)
        .then_some(generation)
}

fn match_model(
    rules: &[BlueFieldModelRule],
    source: ModelSource,
    value: &str,
) -> Option<BlueFieldGeneration> {
    rules
        .iter()
        .find(|rule| {
            rule.sources.contains(&source)
                && rule.model_names.iter().any(|name| name.matches(value))
        })
        .map(|rule| rule.generation)
}

#[cfg(test)]
mod tests;
