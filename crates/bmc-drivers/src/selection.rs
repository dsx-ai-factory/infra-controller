/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Driver selection: platform rules, the driver map they resolve to for
//! one BMC, and the hash that ties a persisted map to the rules that
//! produced it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use blake3::{Hash, Hasher};
use bmc_platform::{
    Capability, IdentityField, IdentityMatcher, MatchPattern, PlatformIdentity, Precedence, Quirk,
    derived_precedence,
};
use carbide_utils::has_duplicates;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::{EnumCount, IntoEnumIterator};
use thiserror::Error;

use crate::drivers::Driver;

/// Which driver serves one capability on one BMC.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilitySelection {
    Standard,
    Unsupported,
    Driver(Driver),
}

/// The selection's wire form: `standard`, `unsupported`, or the driver id.
impl fmt::Display for CapabilitySelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Standard => "standard",
            Self::Unsupported => "unsupported",
            Self::Driver(driver) => driver.as_str(),
        })
    }
}

impl Serialize for CapabilitySelection {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for CapabilitySelection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "standard" => Ok(Self::Standard),
            "unsupported" => Ok(Self::Unsupported),
            _ => value
                .parse()
                .map(Self::Driver)
                .map_err(serde::de::Error::custom),
        }
    }
}

/// The complete per-capability driver selection persisted for one BMC.
///
/// Serializes as an object with one key per capability, in
/// [`Capability`] declaration order; deserialization requires every key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DriverMap([CapabilitySelection; Capability::COUNT]);

impl DriverMap {
    /// A map that selects `selection` for every capability.
    pub fn filled(selection: CapabilitySelection) -> Self {
        Self(std::array::from_fn(|_| selection.clone()))
    }

    /// Returns the selection for `capability`.
    pub fn get(&self, capability: Capability) -> &CapabilitySelection {
        &self.0[capability as usize]
    }

    /// Replaces the selection for `capability`.
    pub fn set(&mut self, capability: Capability, selection: CapabilitySelection) {
        self.0[capability as usize] = selection;
    }

    /// Returns `self` with `capability` switched to `selection`.
    pub fn with(mut self, capability: Capability, selection: CapabilitySelection) -> Self {
        self.set(capability, selection);
        self
    }

    /// Iterates selections in [`Capability`] declaration order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (Capability, &CapabilitySelection)> {
        Capability::iter().zip(&self.0)
    }
}

/// `capability=selection` pairs in [`Capability`] declaration order.
impl fmt::Display for DriverMap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, (capability, selection)) in self.iter().enumerate() {
            if index > 0 {
                formatter.write_str(" ")?;
            }
            write!(formatter, "{capability}={selection}")?;
        }
        Ok(())
    }
}

impl Serialize for DriverMap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (capability, selection) in self.iter() {
            map.serialize_entry(capability.as_str(), selection)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for DriverMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor;

        impl<'de> Visitor<'de> for MapVisitor {
            type Value = DriverMap;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object with one selection per capability")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut slots: [Option<CapabilitySelection>; Capability::COUNT] =
                    Default::default();
                while let Some((capability, selection)) =
                    access.next_entry::<Capability, CapabilitySelection>()?
                {
                    if slots[capability as usize].replace(selection).is_some() {
                        return Err(serde::de::Error::duplicate_field(capability.as_str()));
                    }
                }
                let mut selections = Vec::with_capacity(slots.len());
                for (capability, slot) in Capability::iter().zip(slots) {
                    selections.push(
                        slot.ok_or_else(|| serde::de::Error::missing_field(capability.as_str()))?,
                    );
                }
                selections
                    .try_into()
                    .map(DriverMap)
                    .map_err(|_| serde::de::Error::custom("driver map has the wrong arity"))
            }
        }

        deserializer.deserialize_map(MapVisitor)
    }
}

/// One platform rule: the identity evidence that recognizes it and what
/// that evidence implies for each capability it cares about.
///
/// A rule sets only the capabilities its hardware deviates on; every other
/// capability falls to the compiled default. Precedence is not declared: it
/// is derived from the most specific identity field the rule reads, so a
/// broad rule can never outrank a narrower one. Rules parsed as
/// deployment overrides rank above every built-in rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Stable non-empty identifier used in decisions and ambiguity errors.
    pub id: String,
    /// Predicates that must all match. An empty list is a catch-all rule.
    #[serde(default)]
    pub matchers: Vec<IdentityMatcher>,
    /// The capabilities this rule decides.
    #[serde(default)]
    pub selections: BTreeMap<Capability, CapabilitySelection>,
    /// Firmware quirks this rule's BMCs have, added to those of every other
    /// matching rule.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub quirks: BTreeSet<Quirk>,
    #[serde(skip)]
    deployment_override: bool,
}

impl Rule {
    /// Starts a built-in rule recognized by `matchers`.
    pub fn new(id: impl Into<String>, matchers: impl IntoIterator<Item = IdentityMatcher>) -> Self {
        Self {
            id: id.into(),
            matchers: matchers.into_iter().collect(),
            selections: BTreeMap::new(),
            quirks: BTreeSet::new(),
            deployment_override: false,
        }
    }

    /// Adds firmware quirks; unlike drivers, every matching rule's quirks apply.
    pub fn quirks(mut self, quirks: impl IntoIterator<Item = Quirk>) -> Self {
        self.quirks.extend(quirks);
        self
    }

    /// Selects each driver for the capability it implements.
    ///
    /// Panics when a capability is selected twice or a plugin id is given;
    /// built-in rules are compiled data, so both are programming errors.
    pub fn drivers(mut self, drivers: impl IntoIterator<Item = Driver>) -> Self {
        for driver in drivers {
            let capability = driver
                .capability()
                .expect("built-in rules name compiled drivers");
            self.select(capability, CapabilitySelection::Driver(driver));
        }
        self
    }

    /// Pins capabilities to the standard driver, shadowing broader rules.
    pub fn standard(mut self, capabilities: impl IntoIterator<Item = Capability>) -> Self {
        for capability in capabilities {
            self.select(capability, CapabilitySelection::Standard);
        }
        self
    }

    /// Marks capabilities the hardware cannot provide so callers fail before any I/O.
    pub fn unsupported(mut self, capabilities: impl IntoIterator<Item = Capability>) -> Self {
        for capability in capabilities {
            self.select(capability, CapabilitySelection::Unsupported);
        }
        self
    }

    /// The rank this rule competes at.
    pub fn precedence(&self) -> Precedence {
        if !self.deployment_override {
            return derived_precedence(&self.matchers);
        }
        if self
            .matchers
            .iter()
            .any(|matcher| matcher.field.is_mac_address())
        {
            Precedence::DeploymentBmcOverride
        } else {
            Precedence::DeploymentOverride
        }
    }

    fn select(&mut self, capability: Capability, selection: CapabilitySelection) {
        let previous = self.selections.insert(capability, selection);
        assert!(
            previous.is_none(),
            "rule {} selects {capability} twice",
            self.id
        );
    }

    fn matches(&self, identity: &PlatformIdentity) -> bool {
        self.matchers
            .iter()
            .all(|matcher| matcher.matches(identity))
    }

    /// Specificity within one precedence: a rule that had to satisfy more
    /// identity evidence is the narrower match.
    fn rank(&self) -> (Precedence, usize) {
        (self.precedence(), self.matchers.len())
    }
}

/// Deterministic BLAKE3 digest of a validated, canonical rule set.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SelectionHash([u8; 32]);

impl fmt::Display for SelectionHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(Hash::from_bytes(self.0).to_hex().as_str())
    }
}

impl Serialize for SelectionHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SelectionHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Hash::from_hex(&value)
            .map(|hash| Self(*hash.as_bytes()))
            .map_err(serde::de::Error::custom)
    }
}

/// A validated, canonically ordered set of rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rules {
    rules: Vec<Rule>,
    hash: SelectionHash,
}

impl Rules {
    /// Validates and canonicalizes rules before computing their stable hash.
    pub fn new(mut rules: Vec<Rule>) -> Result<Self, RuleError> {
        for rule in &mut rules {
            if rule.id.trim().is_empty() {
                return Err(RuleError::EmptyId);
            }
            if rule.selections.is_empty() && rule.quirks.is_empty() {
                return Err(RuleError::EmptyRule {
                    id: rule.id.clone(),
                });
            }
            for matcher in &mut rule.matchers {
                if let MatchPattern::OneOf(values) = &mut matcher.pattern {
                    values.sort();
                    if values.is_empty() {
                        return Err(RuleError::EmptyOneOf {
                            id: rule.id.clone(),
                            field: matcher.field.clone(),
                        });
                    }
                    if has_duplicates(values.iter()) {
                        return Err(RuleError::DuplicateOneOfValue {
                            id: rule.id.clone(),
                            field: matcher.field.clone(),
                        });
                    }
                }
                let empty_inventory_id =
                    matches!(&matcher.field, IdentityField::FirmwareInventory(id) if id.is_empty());
                if empty_inventory_id || matcher.pattern.first_value().is_some_and(str::is_empty) {
                    return Err(RuleError::EmptyPattern {
                        id: rule.id.clone(),
                        field: matcher.field.clone(),
                    });
                }
                if matcher.pattern.is_version_pattern() && !matcher.field.is_version() {
                    return Err(RuleError::VersionRangeOnNonFirmwareField {
                        id: rule.id.clone(),
                        field: matcher.field.clone(),
                    });
                }
                if matcher.pattern.is_mac_address_pattern() != matcher.field.is_mac_address() {
                    return Err(RuleError::MacAddressPatternMismatch {
                        id: rule.id.clone(),
                        field: matcher.field.clone(),
                    });
                }
                if matcher.field.is_mac_address() && !rule.deployment_override {
                    return Err(RuleError::MacAddressInCompiledRule {
                        id: rule.id.clone(),
                    });
                }
                if !matcher.pattern.has_parseable_bound() {
                    return Err(RuleError::InvalidVersion {
                        id: rule.id.clone(),
                        field: matcher.field.clone(),
                    });
                }
            }
            rule.matchers.sort();
            if has_duplicates(&rule.matchers) {
                return Err(RuleError::DuplicateMatcher {
                    id: rule.id.clone(),
                });
            }
        }
        rules.sort_by(|left, right| left.id.cmp(&right.id));
        if let Some(pair) = rules.windows(2).find(|pair| pair[0].id == pair[1].id) {
            return Err(RuleError::DuplicateId {
                id: pair[0].id.clone(),
            });
        }
        let hash = hash_rules(&rules);
        Ok(Self { rules, hash })
    }

    /// Layers deployment overrides over compiled-in rules. Every override
    /// ranks above every built-in rule.
    pub fn with_overrides(
        mut built_in: Vec<Rule>,
        overrides: impl IntoIterator<Item = Rule>,
    ) -> Result<Self, RuleError> {
        for mut rule in overrides {
            rule.deployment_override = true;
            built_in.push(rule);
        }
        Self::new(built_in)
    }

    /// Returns rules in canonical identifier order.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Returns the stable digest of this canonical rule set.
    pub const fn hash(&self) -> SelectionHash {
        self.hash
    }

    /// Resolves every capability independently, taking `defaults` (normally
    /// the compiled catalogue's default map) for capabilities no rule sets.
    ///
    /// Among matching rules that set a capability, the highest precedence
    /// wins; within it, the rule with the most matchers wins. Two rules
    /// tied on both are ambiguous.
    pub fn resolve(
        &self,
        identity: &PlatformIdentity,
        defaults: &DriverMap,
    ) -> Result<ResolvedSelection, SelectionError> {
        let matching: Vec<&Rule> = self
            .rules
            .iter()
            .filter(|rule| rule.matches(identity))
            .collect();

        let mut drivers = defaults.clone();
        let mut matched_rules = Vec::new();
        for capability in Capability::iter() {
            let Some(winner) = best(
                matching
                    .iter()
                    .copied()
                    .filter(|rule| rule.selections.contains_key(&capability)),
            )
            .map_err(|(precedence, ids)| SelectionError::Ambiguous {
                capability,
                precedence,
                rule_ids: ids,
            })?
            else {
                continue;
            };
            drivers.set(capability, winner.selections[&capability].clone());
            matched_rules.push(MatchedRule {
                capability,
                rule_id: winner.id.clone(),
            });
        }

        Ok(ResolvedSelection {
            drivers,
            matched_rules,
            quirks: matching
                .iter()
                .flat_map(|rule| rule.quirks.iter().copied())
                .collect(),
            hash: self.hash,
        })
    }
}

/// The single highest-ranked rule, `None` when there are none, or the
/// tied rank and canonically ordered ids when several share the top rank.
fn best<'f>(
    candidates: impl Iterator<Item = &'f Rule>,
) -> Result<Option<&'f Rule>, (Precedence, Vec<String>)> {
    let candidates: Vec<&Rule> = candidates.collect();
    let Some(top) = candidates.iter().map(|rule| rule.rank()).max() else {
        return Ok(None);
    };
    let winners: Vec<&Rule> = candidates
        .into_iter()
        .filter(|rule| rule.rank() == top)
        .collect();
    match winners.as_slice() {
        [winner] => Ok(Some(winner)),
        _ => Err((top.0, winners.iter().map(|rule| rule.id.clone()).collect())),
    }
}

/// Failure while validating a rule set.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RuleError {
    /// A rule identifier is empty or whitespace-only.
    #[error("rule id must not be empty")]
    EmptyId,
    /// Two rules use the same identifier.
    #[error("rule id {id} is used more than once")]
    DuplicateId { id: String },
    /// A rule decides nothing.
    #[error("rule {id} selects no capability and adds no quirk")]
    EmptyRule { id: String },
    /// A version bound does not parse as a version.
    #[error("rule {id} has an unparseable version bound for {field:?}")]
    InvalidVersion { id: String, field: IdentityField },
    /// A matcher contains an empty comparison value.
    #[error("rule {id} has an empty pattern for {field:?}")]
    EmptyPattern { id: String, field: IdentityField },
    /// A rule repeats an identical matcher.
    #[error("rule {id} contains a duplicate matcher")]
    DuplicateMatcher { id: String },
    /// A one-of matcher has no candidate values.
    #[error("rule {id} has an empty one-of matcher for {field:?}")]
    EmptyOneOf { id: String, field: IdentityField },
    /// A one-of matcher repeats a candidate value.
    #[error("rule {id} repeats a one-of value for {field:?}")]
    DuplicateOneOfValue { id: String, field: IdentityField },
    /// A firmware range was attached to a non-version identity field.
    #[error("rule {id} applies a firmware range to non-firmware field {field:?}")]
    VersionRangeOnNonFirmwareField { id: String, field: IdentityField },
    /// A MAC address pattern was attached to another field, or a MAC address
    /// field to another pattern.
    #[error(
        "rule {id} must match {field:?} with a mac_address pattern, and only MAC address fields"
    )]
    MacAddressPatternMismatch { id: String, field: IdentityField },
    /// A compiled rule names individual BMCs by MAC address.
    #[error("rule {id} matches a MAC address, which only deployment overrides may do")]
    MacAddressInCompiledRule { id: String },
}

/// Failure to choose one rule for an identity.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SelectionError {
    /// Several matching rules set this capability at the same rank.
    #[error("ambiguous {capability} selection at precedence {precedence:?}: {rule_ids:?}")]
    Ambiguous {
        capability: Capability,
        precedence: Precedence,
        /// Canonically ordered identifiers of the tied rules.
        rule_ids: Vec<String>,
    },
}

/// The rule that decided one capability.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MatchedRule {
    pub capability: Capability,
    pub rule_id: String,
}

/// The complete driver map, its provenance, and the hash of the rules
/// that produced it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedSelection {
    /// The selected complete driver map.
    pub drivers: DriverMap,
    /// Which rule decided each capability, in capability order; capabilities
    /// using the compiled default are absent.
    pub matched_rules: Vec<MatchedRule>,
    /// Firmware quirks of every matching rule; selections stored before
    /// quirks existed have none.
    #[serde(default)]
    pub quirks: BTreeSet<Quirk>,
    /// Hash of the rule set used for this decision.
    pub hash: SelectionHash,
}

impl ResolvedSelection {
    /// The rule that decided `capability`, or `None` for the compiled default.
    pub fn rule_for(&self, capability: Capability) -> Option<&str> {
        self.matched_rules
            .iter()
            .find(|matched| matched.capability == capability)
            .map(|matched| matched.rule_id.as_str())
    }
}

/// Hashes the canonical (sorted, validated) rules through their wire form,
/// so the digest changes exactly when the serialized set does.
fn hash_rules(rules: &[Rule]) -> SelectionHash {
    let mut hasher = Hasher::new();
    hasher.update(b"bmc-drivers-selection-rules-v1");
    hasher.update(&serde_json::to_vec(rules).expect("validated rules serialize"));
    SelectionHash(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests;
