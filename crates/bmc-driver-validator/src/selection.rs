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

//! Driver selection through the compiled rules, with `--driver` overrides
//! layered on as a deployment-override rule.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::SocketAddr;

use bmc_drivers::{
    CapabilitySelection, Driver, Drivers, ResolvedSelection, Rule, Rules, built_in_rules,
    rules_with_overrides,
};
use bmc_platform::{Capability, IdentityField, IdentityMatcher, MatchPattern, PlatformIdentity};
use bmc_runtime::BmcRef;
use strum::IntoEnumIterator;

use crate::connect::credential_key;

/// The id of the rule `--driver` overrides become.
const OVERRIDE_RULE: &str = "bmc-validate-override";

pub(crate) struct Selection {
    resolved: ResolvedSelection,
    /// What the compiled rules alone select, to explain overrides against.
    built_in: ResolvedSelection,
    built_in_rules: Rules,
    overrides: BTreeMap<Capability, CapabilitySelection>,
}

impl Selection {
    /// Resolves `identity` through the compiled rules plus `overrides`,
    /// rejecting overrides the compiled catalogue cannot serve.
    pub(crate) fn resolve(
        identity: &PlatformIdentity,
        overrides: &[String],
    ) -> Result<Self, String> {
        let mut pins = BTreeMap::new();
        for value in overrides {
            let (capability, selection) = parse_override(value)?;
            if pins.insert(capability, selection).is_some() {
                return Err(format!("--driver pins {capability} more than once"));
            }
        }
        Self::with_pins(identity, pins)
    }

    /// How `identity`, resolved with the same overrides, differs from this
    /// selection: each capability whose driver or deciding rule changes, then
    /// the quirks if they change. Empty when it resolves the same.
    pub(crate) fn changes(&self, identity: &PlatformIdentity) -> Result<Vec<String>, String> {
        let other = Self::with_pins(identity, self.overrides.clone())?.resolved;
        let mut changes: Vec<String> = Capability::iter()
            .filter_map(|capability| {
                let before = decided(&self.resolved, capability);
                let after = decided(&other, capability);
                (before != after).then(|| format!("{capability}: {before} -> {after}"))
            })
            .collect();
        if other.quirks != self.resolved.quirks {
            changes.push(format!(
                "quirks: {:?} -> {:?}",
                self.resolved.quirks, other.quirks
            ));
        }
        Ok(changes)
    }

    fn with_pins(
        identity: &PlatformIdentity,
        pins: BTreeMap<Capability, CapabilitySelection>,
    ) -> Result<Self, String> {
        let defaults = Drivers::default_map();
        let built_in_rules = built_in_rules();
        let built_in = built_in_rules
            .resolve(identity, &defaults)
            .map_err(|error| format!("selection failed: {error}"))?;
        let rules = if pins.is_empty() {
            built_in_rules.clone()
        } else {
            let mut pinned = Rule::new(OVERRIDE_RULE, []);
            pinned.selections = pins.clone();
            rules_with_overrides([pinned])
                .map_err(|error| format!("invalid --driver override: {error}"))?
        };
        Drivers::validate_rules(&rules)
            .map_err(|error| format!("incompatible --driver override: {error}"))?;
        let resolved = rules
            .resolve(identity, &defaults)
            .map_err(|error| format!("selection failed: {error}"))?;
        Drivers::validate_map(&resolved.drivers)
            .map_err(|error| format!("selected driver map is not servable: {error}"))?;

        Ok(Self {
            resolved,
            built_in,
            built_in_rules,
            overrides: pins,
        })
    }

    /// The endpoint `bmc-runtime` connects with, carrying this selection.
    pub(crate) fn endpoint(
        &self,
        address: SocketAddr,
        identity: PlatformIdentity,
    ) -> Result<BmcRef, String> {
        BmcRef::new(address, credential_key(), identity, self.resolved.clone())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn is_supported(&self, capability: Capability) -> bool {
        self.resolved.drivers.get(capability) != &CapabilitySelection::Unsupported
    }

    /// The selected driver's id, `standard`, or `unsupported`.
    pub(crate) fn driver(&self, capability: Capability) -> String {
        label(self.resolved.drivers.get(capability))
    }

    /// Why `capability` has its selection.
    pub(crate) fn reason(&self, capability: Capability) -> String {
        if self.overrides.contains_key(&capability) {
            return format!("--driver override; {}", self.override_note(capability));
        }
        rule_reason(&self.resolved, &self.built_in_rules, capability)
    }

    /// How an overridden capability's pin compares with what the rules select.
    pub(crate) fn override_note(&self, capability: Capability) -> String {
        let selection = self.resolved.drivers.get(capability);
        let built_in = self.built_in.drivers.get(capability);
        let rules_choice = format!(
            "{} ({})",
            label(built_in),
            rule_reason(&self.built_in, &self.built_in_rules, capability)
        );
        if built_in == selection {
            return format!("same as the rules: {rules_choice}");
        }
        let mut note = format!("the rules select {rules_choice}");
        if let CapabilitySelection::Driver(_) = selection {
            let selecting: Vec<&str> = self
                .built_in_rules
                .rules()
                .iter()
                .filter(|rule| rule.selections.get(&capability) == Some(selection))
                .map(|rule| rule.id.as_str())
                .collect();
            if selecting.is_empty() {
                note.push_str("; no compiled rule selects this driver");
            } else {
                let _ = write!(
                    note,
                    "; compiled for rule {}, which does not match this BMC",
                    selecting.join(", ")
                );
            }
        }
        note
    }

    /// What decided `capability`, briefly: a rule id, the compiled default,
    /// or an override.
    pub(crate) fn source(&self, capability: Capability) -> String {
        if self.overrides.contains_key(&capability) {
            return "--driver override".to_string();
        }
        match self.resolved.rule_for(capability) {
            Some(id) => id.to_string(),
            None if self.is_supported(capability) => "default".to_string(),
            None => "no rule, no standard driver".to_string(),
        }
    }

    /// Every capability pinned by `--driver`.
    pub(crate) fn overridden(&self) -> impl Iterator<Item = Capability> + '_ {
        self.overrides.keys().copied()
    }

    /// Each compiled rule that decided a capability, with its matchers, in
    /// the order capabilities first cite them.
    pub(crate) fn deciding_rules(&self) -> Vec<(&str, String)> {
        let mut rules: Vec<(&str, String)> = Vec::new();
        for capability in Capability::iter().filter(|c| !self.overrides.contains_key(c)) {
            let Some(id) = self.resolved.rule_for(capability) else {
                continue;
            };
            if rules.iter().any(|(seen, _)| *seen == id) {
                continue;
            }
            if let Some(rule) = self
                .built_in_rules
                .rules()
                .iter()
                .find(|rule| rule.id == id)
            {
                rules.push((rule.id.as_str(), matchers(rule)));
            }
        }
        rules
    }

    pub(crate) fn quirks(&self) -> Vec<String> {
        self.resolved
            .quirks
            .iter()
            .map(|quirk| format!("{quirk:?}"))
            .collect()
    }

    pub(crate) fn hash(&self) -> String {
        self.resolved.hash.to_string()
    }
}

/// Parses `<capability>=<selection>` or a compiled driver id alone.
fn parse_override(value: &str) -> Result<(Capability, CapabilitySelection), String> {
    let Some((capability, selection)) = value.split_once('=') else {
        let driver: Driver = value
            .parse()
            .map_err(|error| format!("--driver {value}: {error}"))?;
        let capability = driver.capability().ok_or_else(|| {
            format!(
                "--driver {value}: no compiled driver has this id; known ids: {}",
                known_ids()
            )
        })?;
        return Ok((capability, CapabilitySelection::Driver(driver)));
    };
    let capability: Capability = capability
        .parse()
        .map_err(|_| format!("--driver {value}: unknown capability {capability}"))?;
    let selection = match selection {
        "standard" => CapabilitySelection::Standard,
        "unsupported" => CapabilitySelection::Unsupported,
        id => CapabilitySelection::Driver(
            id.parse()
                .map_err(|error| format!("--driver {value}: {error}"))?,
        ),
    };
    Ok((capability, selection))
}

fn known_ids() -> String {
    Driver::ALL
        .iter()
        .map(Driver::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn label(selection: &CapabilitySelection) -> String {
    match selection {
        CapabilitySelection::Standard => "standard".to_string(),
        CapabilitySelection::Unsupported => "unsupported".to_string(),
        CapabilitySelection::Driver(driver) => driver.as_str().to_string(),
    }
}

/// The selection for `capability` and what decided it, e.g.
/// `ami-megarac-bios (rule lenovo-ami)`.
fn decided(resolved: &ResolvedSelection, capability: Capability) -> String {
    let source = resolved
        .rule_for(capability)
        .map_or_else(|| "default".to_string(), |id| format!("rule {id}"));
    format!("{} ({source})", label(resolved.drivers.get(capability)))
}

fn rule_reason(resolved: &ResolvedSelection, rules: &Rules, capability: Capability) -> String {
    let unsupported = resolved.drivers.get(capability) == &CapabilitySelection::Unsupported;
    match resolved.rule_for(capability) {
        Some(id) => {
            let verb = if unsupported {
                "marks it unsupported"
            } else {
                "selects it"
            };
            match rules.rules().iter().find(|rule| rule.id == id) {
                Some(rule) => format!("rule {id} {verb} ({})", matchers(rule)),
                None => format!("rule {id} {verb}"),
            }
        }
        None if resolved.drivers.get(capability) == &CapabilitySelection::Standard => {
            "the compiled default".to_string()
        }
        None => "no rule selects a driver and no standard driver is compiled".to_string(),
    }
}

fn matchers(rule: &Rule) -> String {
    if rule.matchers.is_empty() {
        return "matches every BMC".to_string();
    }
    rule.matchers
        .iter()
        .map(matcher)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A matcher as `field comparison value`, e.g. `oem key = Ami`.
fn matcher(matcher: &IdentityMatcher) -> String {
    let field = match &matcher.field {
        IdentityField::ServiceRootVendor => "vendor".to_string(),
        IdentityField::ServiceRootProduct => "product".to_string(),
        IdentityField::ServiceRootOemKey => "oem key".to_string(),
        IdentityField::ManagerModel => "manager model".to_string(),
        IdentityField::ManagerFirmware => "manager firmware".to_string(),
        IdentityField::ManagerId => "manager id".to_string(),
        IdentityField::SystemId => "system id".to_string(),
        IdentityField::SystemManufacturer => "system manufacturer".to_string(),
        IdentityField::SystemModel => "system model".to_string(),
        IdentityField::SystemSku => "system sku".to_string(),
        IdentityField::SystemPartNumber => "system part".to_string(),
        IdentityField::SystemBiosVersion => "system bios".to_string(),
        IdentityField::ChassisId => "chassis id".to_string(),
        IdentityField::ChassisManufacturer => "chassis manufacturer".to_string(),
        IdentityField::ChassisModel => "chassis model".to_string(),
        IdentityField::ChassisPartNumber => "chassis part".to_string(),
        IdentityField::FirmwareInventory(id) => format!("firmware {id}"),
        IdentityField::ManagerMacAddress => "manager mac".to_string(),
    };
    let comparison = match &matcher.pattern {
        MatchPattern::Exact(value)
        | MatchPattern::ExactAsciiCaseInsensitive(value)
        | MatchPattern::VersionEqual(value) => format!("= {value}"),
        MatchPattern::Prefix(value) => format!("starts with {value}"),
        MatchPattern::Contains(value) | MatchPattern::ContainsAsciiCaseInsensitive(value) => {
            format!("contains {value}")
        }
        MatchPattern::OneOf(values) => format!("in {}", values.join(" | ")),
        MatchPattern::FirmwareVersionRange(range) => format!("in {range:?}"),
        MatchPattern::VersionAtLeast(value) => format!(">= {value}"),
        MatchPattern::VersionBelow(value) => format!("< {value}"),
        MatchPattern::MacAddress(mac_address) => format!("= {mac_address}"),
    };
    format!("{field} {comparison}")
}

#[cfg(test)]
mod tests {
    use bmc_platform::{ServiceRootIdentity, SystemIdentity};

    use super::*;

    fn identity(vendor: &str, oem_keys: &[&str]) -> PlatformIdentity {
        PlatformIdentity {
            service_root: ServiceRootIdentity {
                vendor: Some(vendor.to_string()),
                product: None,
                oem_keys: oem_keys.iter().map(ToString::to_string).collect(),
            },
            ..PlatformIdentity::default()
        }
    }

    /// Rediscovery names each capability whose driver or deciding rule
    /// changed, and the quirks, so a failed reset check shows what moved.
    #[test]
    fn changes_name_each_moved_capability_and_the_quirks() {
        let sr650_v4 = PlatformIdentity {
            system: Some(SystemIdentity {
                model: Some("ThinkSystem SR650 V4".to_string()),
                ..SystemIdentity::default()
            }),
            ..identity("Lenovo", &[])
        };
        let selection = Selection::resolve(&sr650_v4, &[]).unwrap();
        assert_eq!(selection.changes(&sr650_v4), Ok(Vec::new()));
        let changes = selection.changes(&identity("Contoso", &[])).unwrap();
        for expected in [
            "bios: lenovo-xcc-bios (rule lenovo-xcc) -> standard (default)",
            "quirks: {RedfishRestartCutsDpuPower} -> {}",
        ] {
            assert!(
                changes.iter().any(|change| change == expected),
                "{expected:?} missing from {changes:?}"
            );
        }
    }
}
