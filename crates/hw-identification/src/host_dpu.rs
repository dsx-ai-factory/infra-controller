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

//! Identifies DPUs in the inventory reported by a host BMC.
//!
//! [`HostDpuRules::identify_dpus`] identifies cards from host inventory. Each result carries
//! a reference to the [`HostDpuRule`] that matched the reported part number.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use model::site_explorer::{Chassis, EndpointExplorationReport, PCIeDevice};
use serde::{Deserialize, Serialize};

mod defaults;

use defaults::DEFAULT_HOST_DPU_RULES;

use crate::pattern::Pattern;

/// Host identification using site rules in declaration order, followed by built-in rules.
/// The first match for a record determines its product. The TOML value is an array of site rules.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<HostDpuRule>")]
pub struct HostDpuRules(Vec<HostDpuRule>);

/// A DPU identified in host inventory. A recognized card may lack a usable serial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDpu<'report, 'rules> {
    /// Trimmed card serial, preserving the case reported by the host.
    pub serial_number: Option<&'report str>,
    /// Part number exactly as reported by the inventory record that matched.
    pub part_number: &'report str,
    /// PCIe record for this card when available, including its Redfish device identifier.
    pub pcie_device: Option<&'report PCIeDevice>,
    /// First reported NIC firmware version across PCIe records for this card.
    pub firmware_version: Option<&'report str>,
    /// The built-in or site rule that identified the card.
    pub rule: &'rules HostDpuRule,
}

/// A host-reported part number and the rule that recognized it, if any.
#[derive(Clone, Copy, Debug)]
pub struct ReportedPartNumber<'report, 'rules> {
    /// The part number exactly as reported by the host.
    pub value: &'report str,
    /// The rule matching this part number at its inventory source.
    pub rule: Option<&'rules HostDpuRule>,
}

impl HostDpuRules {
    /// Identifies cards by host-reported part number. Repeated observations of a nonblank serial
    /// produce one card, with PCIe taking precedence over chassis inventory.
    /// The caller selects the reporting endpoint. A DPU BMC can report its own card.
    pub fn identify_dpus<'report, 'rules>(
        &'rules self,
        host_report: &'report EndpointExplorationReport,
    ) -> Vec<HostDpu<'report, 'rules>> {
        self.host_records(host_report)
            .into_iter()
            .filter_map(|record| {
                let part_number = record.part_number?;
                Some(HostDpu {
                    serial_number: record.serial_number,
                    part_number: part_number.value,
                    pcie_device: record.pcie_device,
                    firmware_version: record.firmware_version,
                    rule: part_number.rule?,
                })
            })
            .collect()
    }

    /// Host-reported serials in inventory order, with unrecognized chassis identities last.
    /// For repeated serials, use the first recognized part number. If none match,
    /// keep the first record's part number, including a missing value.
    /// Unrecognized chassis part numbers are excluded from mode selection so a
    /// retained chassis serial can still use the DPU BMC part number.
    pub fn serials_with_part_numbers<'report, 'rules>(
        &'rules self,
        report: &'report EndpointExplorationReport,
    ) -> Vec<(&'report str, Option<ReportedPartNumber<'report, 'rules>>)> {
        self.host_records(report)
            .into_iter()
            .filter_map(|record| {
                let part_number = record.part_number.filter(|_| {
                    record.source != InventorySource::Chassis || record.is_identified()
                });
                Some((record.serial_number?, part_number))
            })
            .collect()
    }

    /// Returns the first rule matching this source and part number, checking site rules first.
    pub fn matching_rule(
        &self,
        source: InventorySource,
        part_number: &str,
    ) -> Option<&HostDpuRule> {
        self.0.iter().chain(DEFAULT_HOST_DPU_RULES).find(|rule| {
            rule.sources.contains(&source)
                && rule.part_numbers.iter().any(|p| p.matches(part_number))
        })
    }

    /// Serials that associate a DPU with this chassis for boot-interface ordering.
    /// A recognized DPU chassis supplies its own usable serial. Otherwise, the
    /// serials on its network adapters identify which cards are in the chassis.
    pub fn serials_for_chassis_ordering<'a>(
        &self,
        chassis: &'a Chassis,
    ) -> impl Iterator<Item = &'a str> {
        self.chassis_records(chassis)
            .into_iter()
            .filter(|record| {
                record.source == InventorySource::ChassisNetworkAdapter || record.is_identified()
            })
            .filter_map(|record| record.serial_number)
    }

    /// Whether identification uses only the built-in rules.
    pub fn uses_only_defaults(&self) -> bool {
        self.0.is_empty()
    }
}

/// Identifies a product when any part-number pattern matches a record from any listed source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostDpuRule {
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

impl TryFrom<Vec<HostDpuRule>> for HostDpuRules {
    type Error = String;

    fn try_from(rules: Vec<HostDpuRule>) -> Result<Self, Self::Error> {
        let mut ids = HashSet::new();
        for (index, rule) in DEFAULT_HOST_DPU_RULES
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

#[derive(Clone, Copy)]
struct Observation<'a, 'rules> {
    source: InventorySource,
    serial_number: Option<&'a str>,
    part_number: Option<ReportedPartNumber<'a, 'rules>>,
    pcie_device: Option<&'a PCIeDevice>,
    firmware_version: Option<&'a str>,
}

impl Observation<'_, '_> {
    fn is_identified(&self) -> bool {
        self.part_number.is_some_and(|part| part.rule.is_some())
    }
}

impl HostDpuRules {
    fn host_records<'report, 'rules>(
        &'rules self,
        host_report: &'report EndpointExplorationReport,
    ) -> Vec<Observation<'report, 'rules>> {
        let pcie = host_report
            .systems
            .iter()
            .flat_map(|system| &system.pcie_devices)
            .map(|device| Observation {
                pcie_device: Some(device),
                firmware_version: device.firmware_version.as_deref(),
                ..self.observe(
                    InventorySource::SystemPcieDevice,
                    device.part_number.as_deref(),
                    device.serial_number.as_deref(),
                )
            });
        let mut records = pcie
            .chain(
                host_report
                    .chassis
                    .iter()
                    .flat_map(|chassis| self.chassis_records(chassis)),
            )
            .collect::<Vec<_>>();
        // Additional chassis serials supplement the inventory used for mode selection.
        records.sort_by_key(|record| {
            record.source == InventorySource::Chassis && !record.is_identified()
        });
        let mut positions = HashMap::<&str, usize>::new();
        let mut cards: Vec<Observation> = Vec::new();
        for record in records {
            if let Some(serial) = record.serial_number {
                if let Some(&index) = positions.get(serial) {
                    let previous = &mut cards[index];
                    previous.firmware_version =
                        previous.firmware_version.or(record.firmware_version);
                    if !previous.is_identified() && record.is_identified() {
                        *previous = Observation {
                            pcie_device: record.pcie_device.or(previous.pcie_device),
                            firmware_version: previous.firmware_version,
                            ..record
                        };
                    }
                    continue;
                }
                positions.insert(serial, cards.len());
            }
            cards.push(record);
        }
        cards
    }

    fn chassis_records<'a>(&self, chassis: &'a Chassis) -> Vec<Observation<'a, '_>> {
        let card = self.observe(
            InventorySource::Chassis,
            chassis.part_number.as_deref(),
            chassis.serial_number.as_deref(),
        );
        if card.is_identified() && card.serial_number.is_some() {
            return vec![card];
        }

        let mut records = chassis
            .network_adapters
            .iter()
            .map(|adapter| {
                self.observe(
                    InventorySource::ChassisNetworkAdapter,
                    adapter.part_number.as_deref(),
                    adapter.serial_number.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        // Retain chassis serials for pairing with explored DPU BMCs.
        // An identified chassis without a serial supplies the card only if no adapter identified it.
        if card.serial_number.is_some()
            || (card.is_identified() && !records.iter().any(Observation::is_identified))
        {
            records.push(card);
        }
        records
    }

    fn observe<'a>(
        &self,
        source: InventorySource,
        part_number: Option<&'a str>,
        serial_number: Option<&'a str>,
    ) -> Observation<'a, '_> {
        Observation {
            source,
            serial_number: serial_number.map(str::trim).filter(|s| !s.is_empty()),
            part_number: part_number.map(|value| ReportedPartNumber {
                value,
                rule: self.matching_rule(source, value),
            }),
            pcie_device: None,
            firmware_version: None,
        }
    }
}

#[cfg(test)]
mod tests;
