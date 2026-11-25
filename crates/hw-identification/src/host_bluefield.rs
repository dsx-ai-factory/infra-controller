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

//! Classifies BlueField part numbers by Redfish inventory source.

use std::borrow::Cow;
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

mod defaults;

use defaults::DEFAULT_HOST_BLUEFIELD_RULES;

use crate::pattern::Pattern;

/// Identifies BlueField cards using site rules in declaration order, followed by built-in rules.
/// The first match determines the card's product. The TOML value is an array of site rules.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<HostBlueFieldRule>")]
pub struct HostBlueFieldRules(Vec<HostBlueFieldRule>);

impl HostBlueFieldRules {
    /// Returns the first rule matching this source and part number, checking site rules first.
    pub fn matching_rule(
        &self,
        source: InventorySource,
        part_number: &str,
    ) -> Option<&HostBlueFieldRule> {
        self.0
            .iter()
            .chain(DEFAULT_HOST_BLUEFIELD_RULES)
            .find(|rule| {
                rule.sources.contains(&source)
                    && rule.part_numbers.iter().any(|p| p.matches(part_number))
            })
    }

    /// Returns the product of the first rule matching this source and part number.
    pub fn classify_part_number(
        &self,
        source: InventorySource,
        part_number: &str,
    ) -> Option<DpuProduct> {
        self.matching_rule(source, part_number)
            .map(|rule| rule.product)
    }

    /// Whether identification uses only the built-in rules.
    pub fn uses_only_defaults(&self) -> bool {
        self.0.is_empty()
    }
}

/// Identifies a BlueField product when a part-number pattern matches at a listed source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBlueFieldRule {
    /// Stable, case-sensitive identifier. Must be unique across built-in and site rules.
    pub id: Cow<'static, str>,
    /// Inventory locations eligible for this rule.
    pub sources: Cow<'static, [InventorySource]>,
    /// Alternative patterns for the PartNumber field of one record.
    pub part_numbers: Cow<'static, [Pattern]>,
    /// Product identified by a match.
    pub product: DpuProduct,
}

/// Redfish inventory location that supplies a card's part number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventorySource {
    /// A PCIe device listed by a ComputerSystem.
    SystemPcieDevice,
    /// The Chassis record itself.
    Chassis,
    /// A NetworkAdapter on a chassis that does not itself report a recognized DPU with a usable serial.
    ChassisNetworkAdapter,
    /// The product chassis from a matched DPU's own BMC report.
    DpuProductChassis,
}

/// Factory product identified from a card's part number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DpuProduct {
    /// A BlueField-2 DPU.
    #[serde(rename = "bluefield2_dpu")]
    BlueField2Dpu,
    /// A BlueField-3 DPU.
    #[serde(rename = "bluefield3_dpu")]
    BlueField3Dpu,
    /// A BlueField-3 SuperNIC.
    #[serde(rename = "bluefield3_supernic")]
    BlueField3SuperNic,
    /// BlueField-3 SuperNIC in the 9D3B4 SKU family (the legacy MLX API's BF3 NIC-mode kind).
    #[serde(rename = "bluefield3_supernic_9d3b4")]
    BlueField3SuperNic9D3B4,
    /// A BlueField-4 DPU.
    #[serde(rename = "bluefield4_dpu")]
    BlueField4Dpu,
    /// A BlueField whose product could not be determined.
    #[serde(rename = "unspecified_bluefield")]
    UnspecifiedBlueField,
}

impl TryFrom<Vec<HostBlueFieldRule>> for HostBlueFieldRules {
    type Error = String;

    fn try_from(rules: Vec<HostBlueFieldRule>) -> Result<Self, Self::Error> {
        let mut ids = HashSet::new();
        for (index, rule) in DEFAULT_HOST_BLUEFIELD_RULES
            .iter()
            .enumerate()
            .chain(rules.iter().enumerate())
        {
            let id = rule.id.as_ref();
            if id.is_empty() || id.trim() != id {
                return Err(format!(
                    "invalid host DPU rule ID {id:?} at index {index}: expected a nonempty ID without leading or trailing whitespace"
                ));
            }
            if !ids.insert(id) {
                return Err(format!("duplicate host DPU rule ID {id:?}"));
            }
            if rule.sources.is_empty() || rule.part_numbers.is_empty() {
                return Err(format!(
                    "host DPU rule {id:?} requires at least one source and part-number pattern"
                ));
            }
            if rule
                .part_numbers
                .iter()
                .any(|pattern| pattern.text().trim().is_empty())
            {
                return Err(format!(
                    "host DPU rule {id:?} has a blank part-number pattern"
                ));
            }
        }
        Ok(Self(rules))
    }
}

#[cfg(test)]
mod tests;
