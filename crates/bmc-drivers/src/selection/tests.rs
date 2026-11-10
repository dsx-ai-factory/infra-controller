/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use bmc_platform::{
    Capability, ChassisIdentity, FirmwareInventoryIdentity, FirmwareVersionRange, IdentityField,
    IdentityMatcher, ManagerIdentity, MatchPattern, Precedence, ServiceRootIdentity,
    SystemIdentity,
};
use mac_address::MacAddress;

use super::*;

fn identity() -> PlatformIdentity {
    PlatformIdentity {
        service_root: ServiceRootIdentity {
            vendor: Some("NVIDIA".to_string()),
            product: Some("GB BMC".to_string()),
            oem_keys: vec!["Nvidia".to_string()],
        },
        manager: Some(ManagerIdentity {
            id: "BMC_0".to_string(),
            model: Some("OpenBMC".to_string()),
            firmware: Some("1.2.3".to_string()),
            mac_addresses: vec![bmc_mac()],
        }),
        system: Some(SystemIdentity {
            id: "System_0".to_string(),
            manufacturer: Some("NVIDIA".to_string()),
            model: Some("GB300".to_string()),
            sku: Some("DGX".to_string()),
            part_number: Some("900-2G535".to_string()),
            bios_version: Some("2.0".to_string()),
        }),
        chassis: vec![ChassisIdentity {
            id: "GPU_Chassis".to_string(),
            manufacturer: Some("NVIDIA".to_string()),
            model: Some("NVIDIA GB300".to_string()),
            part_number: None,
        }],
        firmware_inventory: vec![
            FirmwareInventoryIdentity {
                id: "HostBIOS_0".to_string(),
                version: Some("1.01.03".to_string()),
            },
            FirmwareInventoryIdentity {
                id: "HostBMC_0".to_string(),
                version: Some("23.11.21".to_string()),
            },
        ],
    }
}

fn vendor(value: &str) -> IdentityMatcher {
    IdentityMatcher::new(
        IdentityField::ServiceRootVendor,
        MatchPattern::Exact(value.to_string()),
    )
}

fn bmc_mac() -> MacAddress {
    "C4:5A:B1:C8:38:0D".parse().expect("fixture MAC parses")
}

fn this_bmc() -> IdentityMatcher {
    IdentityMatcher::new(
        IdentityField::ManagerMacAddress,
        MatchPattern::MacAddress(bmc_mac()),
    )
}

fn model(value: &str) -> IdentityMatcher {
    IdentityMatcher::new(
        IdentityField::SystemModel,
        MatchPattern::Exact(value.to_string()),
    )
}

/// A rule that pins power to the standard driver.
fn rule(id: &str, matchers: impl IntoIterator<Item = IdentityMatcher>) -> Rule {
    Rule::new(id, matchers).standard([Capability::Power])
}

/// The defaults the compiled catalogue would supply.
fn defaults() -> DriverMap {
    DriverMap::filled(CapabilitySelection::Standard)
        .with(Capability::Lockdown, CapabilitySelection::Unsupported)
        .with(Capability::Storage, CapabilitySelection::Unsupported)
        .with(Capability::Dpu, CapabilitySelection::Unsupported)
        .with(Capability::Console, CapabilitySelection::Unsupported)
}

#[test]
fn one_of_matches_any_exact_candidate_and_canonicalizes_order() {
    let one_of = |values: &[&str]| {
        IdentityMatcher::new(
            IdentityField::SystemModel,
            MatchPattern::OneOf(values.iter().map(|value| (*value).to_string()).collect()),
        )
    };
    assert!(one_of(&["GB200", "GB300"]).matches(&identity()));

    let left = Rules::new(vec![rule("models", [one_of(&["GB200", "GB300"])])])
        .expect("one-of values are valid");
    let right = Rules::new(vec![rule("models", [one_of(&["GB300", "GB200"])])])
        .expect("reordered one-of values are valid");
    assert_eq!(left.hash(), right.hash());
}

#[test]
fn rule_validation_rejects_malformed_declarations() {
    let invalid = |matcher: IdentityMatcher| vec![rule("invalid", [matcher])];
    let cases = [
        (
            invalid(IdentityMatcher::new(
                IdentityField::SystemModel,
                MatchPattern::OneOf(Vec::new()),
            )),
            RuleError::EmptyOneOf {
                id: "invalid".to_string(),
                field: IdentityField::SystemModel,
            },
        ),
        (
            invalid(IdentityMatcher::new(
                IdentityField::SystemModel,
                MatchPattern::OneOf(vec!["GB300".to_string(), "GB300".to_string()]),
            )),
            RuleError::DuplicateOneOfValue {
                id: "invalid".to_string(),
                field: IdentityField::SystemModel,
            },
        ),
        (
            invalid(IdentityMatcher::new(
                IdentityField::SystemModel,
                MatchPattern::FirmwareVersionRange(
                    FirmwareVersionRange::new("1.0".to_string(), "2.0".to_string())
                        .expect("fixture range is valid"),
                ),
            )),
            RuleError::VersionRangeOnNonFirmwareField {
                id: "invalid".to_string(),
                field: IdentityField::SystemModel,
            },
        ),
        (
            invalid(IdentityMatcher::version_below(
                IdentityField::SystemModel,
                "2.0",
            )),
            RuleError::VersionRangeOnNonFirmwareField {
                id: "invalid".to_string(),
                field: IdentityField::SystemModel,
            },
        ),
        (
            invalid(IdentityMatcher::version_at_least(
                IdentityField::ManagerFirmware,
                "not a version",
            )),
            RuleError::InvalidVersion {
                id: "invalid".to_string(),
                field: IdentityField::ManagerFirmware,
            },
        ),
        (
            invalid(IdentityMatcher::version_at_least(
                IdentityField::FirmwareInventory(String::new()),
                "1.0",
            )),
            RuleError::EmptyPattern {
                id: "invalid".to_string(),
                field: IdentityField::FirmwareInventory(String::new()),
            },
        ),
        (
            invalid(model("")),
            RuleError::EmptyPattern {
                id: "invalid".to_string(),
                field: IdentityField::SystemModel,
            },
        ),
        (
            vec![rule("duplicate-matcher", [model("GB300"), model("GB300")])],
            RuleError::DuplicateMatcher {
                id: "duplicate-matcher".to_string(),
            },
        ),
        (vec![rule("", [model("GB300")])], RuleError::EmptyId),
        (
            vec![
                rule("duplicate", [model("GB300")]),
                rule("duplicate", [model("GB300")]),
            ],
            RuleError::DuplicateId {
                id: "duplicate".to_string(),
            },
        ),
        (
            vec![Rule::new("empty", [model("GB300")])],
            RuleError::EmptyRule {
                id: "empty".to_string(),
            },
        ),
        (
            invalid(IdentityMatcher::new(
                IdentityField::SystemModel,
                MatchPattern::MacAddress(bmc_mac()),
            )),
            RuleError::MacAddressPatternMismatch {
                id: "invalid".to_string(),
                field: IdentityField::SystemModel,
            },
        ),
        (
            invalid(IdentityMatcher::new(
                IdentityField::ManagerMacAddress,
                MatchPattern::Exact("C4:5A:B1:C8:38:0D".to_string()),
            )),
            RuleError::MacAddressPatternMismatch {
                id: "invalid".to_string(),
                field: IdentityField::ManagerMacAddress,
            },
        ),
        (
            invalid(this_bmc()),
            RuleError::MacAddressInCompiledRule {
                id: "invalid".to_string(),
            },
        ),
    ];
    for (rules, expected) in cases {
        assert_eq!(Rules::new(rules), Err(expected));
    }
}

#[test]
fn highest_ranked_rule_selects_the_capability() {
    let oem = IdentityMatcher::new(
        IdentityField::ServiceRootOemKey,
        MatchPattern::Exact("Nvidia".to_string()),
    );
    let site: Rule = toml::from_str(
        r#"
        id = "site"
        matchers = [{ field = "system_model", pattern = { kind = "contains", value = "GB300" } }]
        selections = { power = "unsupported" }
    "#,
    )
    .expect("override rule deserializes");
    let cases = [
        (
            "higher precedence wins",
            Rules::new(vec![
                rule("specific", [vendor("NVIDIA")]),
                Rule::new("fallback", []).unsupported([Capability::Power]),
            ]),
            "specific",
            CapabilitySelection::Standard,
        ),
        (
            "more matchers win within one precedence",
            Rules::new(vec![
                Rule::new("specific", [vendor("NVIDIA"), oem]).unsupported([Capability::Power]),
                rule("broad", [vendor("NVIDIA")]),
            ]),
            "specific",
            CapabilitySelection::Unsupported,
        ),
        (
            "overrides outrank built-ins",
            Rules::with_overrides(vec![rule("vendor", [vendor("NVIDIA")])], [site]),
            "site",
            CapabilitySelection::Unsupported,
        ),
        (
            "an override naming the BMC outranks one with more matchers",
            Rules::with_overrides(
                Vec::new(),
                [
                    Rule::new("site-class", [vendor("NVIDIA"), model("GB300")])
                        .unsupported([Capability::Power]),
                    rule("site-this-bmc", [this_bmc()]),
                ],
            ),
            "site-this-bmc",
            CapabilitySelection::Standard,
        ),
    ];
    for (scenario, rules, winner, selection) in cases {
        let resolved = rules
            .expect(scenario)
            .resolve(&identity(), &defaults())
            .expect(scenario);
        assert_eq!(
            resolved.rule_for(Capability::Power),
            Some(winner),
            "{scenario}"
        );
        assert_eq!(
            resolved.drivers.get(Capability::Power),
            &selection,
            "{scenario}"
        );
    }
}

#[test]
fn capabilities_resolve_independently_across_rules() {
    let rules = Rules::new(vec![
        Rule::new("nvidia", [vendor("NVIDIA")]).drivers([Driver::NvidiaOpenBmcPower]),
        Rule::new("gb300", [model("GB300")]).unsupported([Capability::Accounts]),
    ])
    .expect("rules are valid");

    let resolved = rules
        .resolve(&identity(), &defaults())
        .expect("capabilities resolve");

    assert_eq!(resolved.rule_for(Capability::Power), Some("nvidia"));
    assert_eq!(resolved.rule_for(Capability::Accounts), Some("gb300"));
    assert_eq!(resolved.rule_for(Capability::Bios), None);
    assert_eq!(
        resolved.drivers.get(Capability::Power),
        &CapabilitySelection::Driver(Driver::NvidiaOpenBmcPower)
    );
    assert_eq!(
        resolved.drivers.get(Capability::Accounts),
        &CapabilitySelection::Unsupported
    );
    assert_eq!(
        resolved.drivers.get(Capability::Bios),
        &CapabilitySelection::Standard
    );
}

#[test]
fn tied_highest_rank_is_ambiguous_in_canonical_order() {
    let rules = Rules::new(vec![
        rule("z-rule", [model("GB300")]),
        rule("a-rule", [model("GB300")]),
    ])
    .expect("rules are valid");

    assert_eq!(
        rules.resolve(&identity(), &defaults()),
        Err(SelectionError::Ambiguous {
            capability: Capability::Power,
            precedence: Precedence::ExactSystemIdentity,
            rule_ids: vec!["a-rule".to_string(), "z-rule".to_string()],
        })
    );
}

#[test]
fn hash_is_order_independent_and_semantically_sensitive() {
    let first = rule("nvidia", [vendor("NVIDIA")]);
    let second = rule("gb300", [model("GB300")]);
    let forward = Rules::new(vec![first.clone(), second.clone()]).expect("rules are valid");
    let reverse = Rules::new(vec![second, first.clone()]).expect("rules are valid");
    assert_eq!(forward.hash(), reverse.hash());

    let changed = Rule::new("gb300", [model("GB300")]).unsupported([Capability::Power]);
    let changed = Rules::new(vec![first, changed]).expect("changed rules remain valid");
    assert_ne!(forward.hash(), changed.hash());

    let encoded = serde_json::to_string(&forward.hash()).expect("hash serializes");
    assert_eq!(encoded.len(), 66);
    assert_eq!(
        serde_json::from_str::<SelectionHash>(&encoded).expect("hash deserializes"),
        forward.hash()
    );
}

#[test]
fn no_matching_rule_uses_each_capabilitys_compiled_default() {
    let rules = Rules::new(vec![rule("dell", [vendor("Dell")])]).expect("rules are valid");
    let resolved = rules
        .resolve(&identity(), &defaults())
        .expect("defaults resolve");
    assert!(resolved.matched_rules.is_empty());
    assert_eq!(resolved.drivers, defaults());
}
