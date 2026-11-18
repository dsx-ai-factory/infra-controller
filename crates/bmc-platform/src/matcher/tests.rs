/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_test_support::value_scenarios;

use super::*;
use crate::{
    ChassisIdentity, FirmwareInventoryIdentity, ManagerIdentity, ServiceRootIdentity,
    SystemIdentity,
};

fn mac(value: &str) -> MacAddress {
    value.parse().expect("fixture MAC parses")
}

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
            mac_addresses: vec![mac("C4:5A:B1:C8:38:0D"), mac("C4:5A:B1:C8:38:0E")],
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

#[test]
fn match_patterns_cover_every_comparison_mode() {
    value_scenarios!(run = |pattern: MatchPattern| pattern.matches("NVIDIA GB300");
        "matching patterns" {
            MatchPattern::Exact("NVIDIA GB300".to_string()) => true,
            MatchPattern::ExactAsciiCaseInsensitive("nvidia gb300".to_string()) => true,
            MatchPattern::Prefix("NVIDIA".to_string()) => true,
            MatchPattern::Contains("GB300".to_string()) => true,
            MatchPattern::ContainsAsciiCaseInsensitive("nvidia gb".to_string()) => true,
        }
        "non-matching patterns" {
            MatchPattern::Exact("NVIDIA".to_string()) => false,
            MatchPattern::ExactAsciiCaseInsensitive("nvidia gb200".to_string()) => false,
            MatchPattern::Prefix("GB300".to_string()) => false,
            MatchPattern::Contains("GB200".to_string()) => false,
        }
    );
}

#[test]
fn matchers_read_scalar_and_repeated_identity_fields() {
    let cases = [
        (
            IdentityField::ServiceRootVendor,
            MatchPattern::Exact("NVIDIA".to_string()),
            true,
        ),
        (
            IdentityField::ManagerModel,
            MatchPattern::Exact("OpenBMC".to_string()),
            true,
        ),
        (
            IdentityField::SystemPartNumber,
            MatchPattern::Prefix("900-".to_string()),
            true,
        ),
        (
            IdentityField::ChassisModel,
            MatchPattern::Contains("GB300".to_string()),
            true,
        ),
        (
            IdentityField::ChassisPartNumber,
            MatchPattern::Exact("missing".to_string()),
            false,
        ),
        (
            IdentityField::FirmwareInventory("HostBIOS_0".to_string()),
            MatchPattern::VersionAtLeast("1.01.03".to_string()),
            true,
        ),
        (
            IdentityField::FirmwareInventory("HostBIOS_0".to_string()),
            MatchPattern::VersionAtLeast("23.11.09".to_string()),
            false,
        ),
        (
            IdentityField::FirmwareInventory("HGX_FW_BMC_0".to_string()),
            MatchPattern::VersionAtLeast("1.0".to_string()),
            false,
        ),
        (
            IdentityField::ManagerMacAddress,
            MatchPattern::MacAddress(mac("c4-5a-b1-c8-38-0e")),
            true,
        ),
        (
            IdentityField::ManagerMacAddress,
            MatchPattern::MacAddress(mac("C4:5A:B1:C8:38:0F")),
            false,
        ),
        (
            IdentityField::ManagerMacAddress,
            MatchPattern::Exact("C4:5A:B1:C8:38:0D".to_string()),
            false,
        ),
    ];
    let identity = identity();
    for (field, pattern, expected) in cases {
        assert_eq!(
            IdentityMatcher::new(field, pattern).matches(&identity),
            expected
        );
    }
}

#[test]
fn firmware_ranges_are_validated_and_inclusive() {
    let range = FirmwareVersionRange::new("1.2.3".to_string(), "2.0".to_string())
        .expect("ordered versions are valid");
    value_scenarios!(run = |version| range.contains(version);
        "inside inclusive range" {
            "1.2.3" => true,
            "1.5" => true,
            "2.0" => true,
        }
        "outside or invalid" {
            "1.2.2" => false,
            "2.0.1" => false,
            "" => false,
        }
    );
    assert_eq!(
        FirmwareVersionRange::new("2.0".to_string(), "1.0".to_string()),
        Err(FirmwareVersionRangeError::Reversed)
    );
}

#[test]
fn precedence_order_matches_selection_design() {
    assert!(
        Precedence::StandardDefault < Precedence::VendorManufacturer
            && Precedence::VendorManufacturer < Precedence::BmcProductManager
            && Precedence::BmcProductManager < Precedence::ExactSystemIdentity
            && Precedence::ExactSystemIdentity < Precedence::DeploymentOverride
            && Precedence::DeploymentOverride < Precedence::DeploymentBmcOverride
    );
}
