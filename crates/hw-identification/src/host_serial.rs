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

//! Serial number sources used to identify compute trays in host inventory.

use std::borrow::Cow;
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

mod defaults;

use defaults::DEFAULT_HOST_SERIAL_RULES;

use crate::hw::{nvidia_openbmc, nvidia_titania_bianca_pdb, nvidia_vr_nvl_pdb};
use crate::pattern::Pattern;

/// Ordered site overrides for the source of a host's inventory serial number.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<HostSerialRule>")]
pub struct HostSerialRules(Vec<HostSerialRule>);

/// Selects a serial source for hosts whose reported hardware class matches the rule.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSerialRule {
    /// Unique, case-sensitive rule identifier.
    pub id: Cow<'static, str>,
    /// Optional pattern on the exploration report's hardware class. Omit to match every host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_class: Option<Pattern>,
    /// Redfish field to read from the stored report.
    pub source: HostSerialSource,
}

/// A serial-bearing field in a host exploration report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostSerialSource {
    /// SerialNumber on the primary ComputerSystem.
    SystemSerial {},
    /// SKU on the primary ComputerSystem.
    SystemSku {},
    /// SerialNumber on a chassis whose Redfish ID matches the pattern.
    Chassis {
        /// Pattern on the Redfish chassis ID.
        chassis_id: Pattern,
    },
    /// SerialNumber on a member of this chassis's Assembly. All supplied patterns must match.
    ChassisAssembly {
        /// Pattern on the Redfish chassis IDs whose Assembly links exploration must follow.
        chassis_id: Pattern,
        /// Pattern on the FRU's Name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<Pattern>,
        /// Pattern on the FRU's Model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<Pattern>,
        /// Pattern on the FRU's MemberId.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        member_id: Option<Pattern>,
    },
}

impl HostSerialRules {
    /// Whether no site overrides were configured.
    pub fn uses_only_defaults(&self) -> bool {
        self.0.is_empty()
    }

    fn rules(&self) -> impl Iterator<Item = &HostSerialRule> {
        self.0.iter().chain(DEFAULT_HOST_SERIAL_RULES)
    }

    /// Site overrides in declaration order for this hardware class.
    pub fn site_sources<'a>(
        &'a self,
        hardware_class: Option<&'a str>,
    ) -> impl Iterator<Item = &'a HostSerialSource> {
        Self::matching_sources(&self.0, hardware_class)
    }

    /// Built-in inventory sources for hosts without a usable site override.
    pub fn built_in_sources(
        hardware_class: Option<&str>,
    ) -> impl Iterator<Item = &HostSerialSource> {
        Self::matching_sources(DEFAULT_HOST_SERIAL_RULES, hardware_class)
    }

    fn matching_sources<'a>(
        rules: &'a [HostSerialRule],
        hardware_class: Option<&'a str>,
    ) -> impl Iterator<Item = &'a HostSerialSource> {
        rules
            .iter()
            .filter(move |rule| {
                rule.hardware_class.as_ref().is_none_or(|pattern| {
                    hardware_class.is_some_and(|class| pattern.matches(class))
                })
            })
            .map(|rule| &rule.source)
    }

    /// Whether any assembly rule needs this chassis. Its host class is discovered after collection.
    pub fn needs_chassis_assembly(&self, id: &str) -> bool {
        self.rules().any(|rule| {
            matches!(&rule.source, HostSerialSource::ChassisAssembly { chassis_id, .. } if chassis_id.matches(id))
        })
    }
}

impl TryFrom<Vec<HostSerialRule>> for HostSerialRules {
    type Error = String;

    fn try_from(rules: Vec<HostSerialRule>) -> Result<Self, Self::Error> {
        let mut ids = HashSet::new();
        for (index, rule) in DEFAULT_HOST_SERIAL_RULES
            .iter()
            .enumerate()
            .chain(rules.iter().enumerate())
        {
            let id = rule.id.as_ref();
            if id.is_empty() || id.trim() != id {
                return Err(format!(
                    "invalid host serial rule ID {id:?} at index {index}: expected a nonempty ID without leading or trailing whitespace"
                ));
            }
            if !ids.insert(id) {
                return Err(format!("duplicate host serial rule ID {id:?}"));
            }
            if rule
                .hardware_class
                .as_ref()
                .is_some_and(|p| p.text().trim().is_empty())
            {
                return Err(format!(
                    "host serial rule {id:?} has a blank hardware class pattern"
                ));
            }
            let chassis_id = match &rule.source {
                HostSerialSource::SystemSerial {} | HostSerialSource::SystemSku {} => continue,
                HostSerialSource::Chassis { chassis_id } => chassis_id,
                HostSerialSource::ChassisAssembly {
                    chassis_id,
                    name,
                    model,
                    member_id,
                } => {
                    if name.is_none() && model.is_none() && member_id.is_none() {
                        return Err(format!(
                            "host serial rule {id:?} requires at least one assembly selector"
                        ));
                    }
                    if [name, model, member_id]
                        .into_iter()
                        .filter_map(Option::as_ref)
                        .any(|p| p.text().trim().is_empty())
                    {
                        return Err(format!(
                            "host serial rule {id:?} has a blank assembly selector"
                        ));
                    }
                    chassis_id
                }
            };
            if chassis_id.text().trim().is_empty() {
                return Err(format!(
                    "host serial rule {id:?} has a blank chassis_id pattern"
                ));
            }
        }
        Ok(Self(rules))
    }
}

/// Chassis whose Assembly members are used by the legacy RPC serial projection.
pub const HOST_SERIAL_ASSEMBLY_CHASSIS_IDS: &[&str] = &[nvidia_openbmc::COMPUTE_TRAY_CHASSIS_ID];

/// Identifies the assembly used for the GB200 or Vera Rubin compute tray's inventory serial.
pub fn is_compute_tray_assembly(chassis_id: &str, assembly_model: Option<&str>) -> bool {
    HOST_SERIAL_ASSEMBLY_CHASSIS_IDS.contains(&chassis_id)
        && matches!(
            assembly_model,
            Some(
                nvidia_openbmc::GB200_PDB_CHASSIS_ASSEMBLY_MODEL
                    | nvidia_titania_bianca_pdb::FRU_PRODUCT_NAME
                    | nvidia_vr_nvl_pdb::CHASSIS_FRU_ASSEMBLY_MODEL
            )
        )
}

#[cfg(test)]
mod tests;
