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

//! Builds RMS node identity from NICo rack profiles.
//!
//! NICo validates only descriptor inputs: a non-empty role-specific vendor and
//! product family. RMS remains responsible for deciding whether a descriptor is
//! supported. A legacy [`rms::NodeType`] is included only for combinations known
//! to this NICo version and never limits descriptor construction.
//!
//! Custom attribute keys and values are copied without normalization, and key
//! matching is case-sensitive. Rack attributes are inherited by every role,
//! role attributes override identical rack keys, and generated `role`, `vendor`,
//! and `product_family` attributes have highest priority. Custom attributes
//! cannot supply missing required named fields.

use std::collections::HashMap;

use librms::protos::rack_manager as rms;
use model::rack_type::{RackProfile, RackProfileConfig};

const KEY_ROLE: &str = "role";
const KEY_VENDOR: &str = "vendor";
const KEY_PRODUCT_FAMILY: &str = "product_family";
const ROLE_COMPUTE: &str = "compute";
const ROLE_SWITCH: &str = "switch";
const ROLE_POWER_SHELF: &str = "power_shelf";
const RMS_NODE_ROLES: [RmsNodeRole; 3] = [
    RmsNodeRole::Compute,
    RmsNodeRole::Switch,
    RmsNodeRole::PowerShelf,
];

/// Error returned when a rack profile lacks data required for an RMS descriptor.
///
/// These errors describe missing local inputs, not unsupported hardware. RMS
/// validates the resulting role, vendor, and product-family combination.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NodeDescriptorError {
    /// The rack profile does not identify a product family needed by RMS.
    #[error("rack profile does not identify an RMS product family")]
    MissingProductFamily,

    /// The rack profile does not identify a vendor for the node role.
    #[error("rack profile does not identify an RMS {role} vendor")]
    VendorMissing { role: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RmsNodeRole {
    Compute,
    Switch,
    PowerShelf,
}

impl RmsNodeRole {
    fn descriptor_value(self) -> &'static str {
        match self {
            Self::Compute => ROLE_COMPUTE,
            Self::Switch => ROLE_SWITCH,
            Self::PowerShelf => ROLE_POWER_SHELF,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Compute => "compute",
            Self::Switch => "switch",
            Self::PowerShelf => "power shelf",
        }
    }
}

/// RMS node identity derived from a rack profile and component role.
///
/// Every identity contains rack-level custom attributes, role-level custom
/// attributes, and the generated `role`, `vendor`, and `product_family`
/// attributes, in increasing precedence order. Known hardware may also carry a
/// legacy enum override for compatibility with RMS versions that predate
/// descriptor dispatch.
#[derive(Clone, Debug, PartialEq)]
pub struct RmsNodeIdentity {
    role: RmsNodeRole,
    legacy_node_type: Option<rms::NodeType>,
    node_descriptor: rms::NodeDescriptor,
}

impl RmsNodeIdentity {
    /// Applies this identity to an RMS node request.
    ///
    /// The descriptor is always populated. The legacy `NodeType` is populated
    /// only when NICo has an exact compatibility mapping; otherwise it remains
    /// unspecified so RMS resolves the descriptor.
    pub fn apply_to_node_info(&self, node: &mut rms::NodeInfo) {
        node.r#type = self.legacy_node_type.map(|node_type| node_type as i32);
        node.node_descriptor = Some(self.node_descriptor.clone());
    }

    /// Returns whether NICo should include switch host endpoint data.
    pub(crate) fn is_switch(&self) -> bool {
        matches!(self.role, RmsNodeRole::Switch)
    }
}

/// Builds the RMS compute identity for a rack profile.
///
/// # Errors
///
/// Returns [`NodeDescriptorError`] when `product_family` or the compute vendor
/// is missing or empty.
pub fn compute_node_identity_for_profile(
    profile: &RackProfile,
) -> Result<RmsNodeIdentity, NodeDescriptorError> {
    node_identity_for_profile(profile, RmsNodeRole::Compute)
}

/// Builds the RMS switch identity for a rack profile.
///
/// # Errors
///
/// Returns [`NodeDescriptorError`] when `product_family` or the switch vendor
/// is missing or empty.
pub fn switch_node_identity_for_profile(
    profile: &RackProfile,
) -> Result<RmsNodeIdentity, NodeDescriptorError> {
    node_identity_for_profile(profile, RmsNodeRole::Switch)
}

/// Builds the RMS power-shelf identity for a rack profile.
///
/// # Errors
///
/// Returns [`NodeDescriptorError`] when `product_family` or the power-shelf
/// vendor is missing or empty.
pub fn power_shelf_node_identity_for_profile(
    profile: &RackProfile,
) -> Result<RmsNodeIdentity, NodeDescriptorError> {
    node_identity_for_profile(profile, RmsNodeRole::PowerShelf)
}

/// Warns about custom RMS descriptor attributes overridden during inheritance.
///
/// Callers should invoke this once after configuration-provider merging so the
/// warnings describe the effective profiles. Role attributes override rack
/// attributes, then the generated `role` and present, non-blank `vendor` and
/// `product_family` fields override custom attributes. Each call emits all
/// collisions without deduplication, and collisions are intentionally non-fatal.
pub fn warn_rms_node_descriptor_attribute_overrides(config: &RackProfileConfig) {
    for (rack_profile_id, profile) in &config.rack_profiles {
        for role in RMS_NODE_ROLES {
            let (vendor, role_attributes) = vendor_and_attributes_for_role(profile, role);

            // Role-level values replace rack-level values with the same exact key.
            for key in role_attributes
                .keys()
                .filter(|key| profile.attributes.contains_key(*key))
            {
                tracing::warn!(
                    rack_profile_id,
                    role = role.descriptor_value(),
                    attribute_key = key,
                    overridden_source = "rack",
                    winning_source = "role",
                    "RMS node descriptor attribute is overridden"
                );
            }

            // Only present named fields override custom values. Custom
            // attributes cannot satisfy missing required named fields.
            let named_attribute_keys = [
                (KEY_ROLE, true),
                (
                    KEY_VENDOR,
                    vendor
                        .map(str::trim)
                        .is_some_and(|vendor| !vendor.is_empty()),
                ),
                (
                    KEY_PRODUCT_FAMILY,
                    profile
                        .product_family
                        .as_ref()
                        .is_some_and(|family| !family.as_str().is_empty()),
                ),
            ];

            for (key, is_present) in named_attribute_keys {
                if !is_present {
                    continue;
                }

                let winning_custom_source = if role_attributes.contains_key(key) {
                    Some("role")
                } else if profile.attributes.contains_key(key) {
                    Some("rack")
                } else {
                    None
                };

                if let Some(winning_custom_source) = winning_custom_source {
                    tracing::warn!(
                        rack_profile_id,
                        role = role.descriptor_value(),
                        attribute_key = key,
                        overridden_source = winning_custom_source,
                        winning_source = "named_field",
                        "RMS node descriptor attribute is overridden"
                    );
                }
            }
        }
    }
}

/// Builds firmware-object component filters for RMS node identities.
///
/// The first tuple element contains compatibility filters keyed by legacy
/// `NodeType`; the second contains descriptor-keyed filters for every identity.
/// Empty `components` produces two empty collections. Unsupported legacy
/// combinations are omitted from the first collection without rejecting the
/// descriptor filter.
pub fn firmware_object_component_filters_for_node_identities<'a>(
    components: &[String],
    node_identities: impl IntoIterator<Item = &'a RmsNodeIdentity>,
) -> (
    HashMap<i32, rms::FirmwareObjectComponentFilter>,
    Vec<rms::NodeDescriptorFirmwareObjectComponentFilter>,
) {
    if components.is_empty() {
        return (HashMap::new(), Vec::new());
    }

    let mut component_filters = HashMap::new();
    let mut descriptor_component_filters = Vec::new();

    for node_identity in node_identities {
        let component_filter = rms::FirmwareObjectComponentFilter {
            components: components.to_vec(),
        };

        if let Some(node_type) = node_identity.legacy_node_type {
            component_filters.insert(node_type as i32, component_filter.clone());
        }

        descriptor_component_filters.push(rms::NodeDescriptorFirmwareObjectComponentFilter {
            node_descriptor: Some(node_identity.node_descriptor.clone()),
            component_filter: Some(component_filter),
        });
    }

    (component_filters, descriptor_component_filters)
}

fn node_identity_for_profile(
    profile: &RackProfile,
    role: RmsNodeRole,
) -> Result<RmsNodeIdentity, NodeDescriptorError> {
    let Some(product_family) = profile
        .product_family
        .as_ref()
        .map(|family| family.as_str())
        .filter(|family| !family.is_empty())
    else {
        return Err(NodeDescriptorError::MissingProductFamily);
    };

    let (vendor, role_attributes) = vendor_and_attributes_for_role(profile, role);

    let Some(vendor) = vendor.map(str::trim).filter(|vendor| !vendor.is_empty()) else {
        return Err(NodeDescriptorError::VendorMissing { role: role.label() });
    };

    // HashMap collection keeps the last value for duplicate keys. Iterator
    // order therefore implements rack, role, then generated-field precedence
    // while leaving custom keys and values unchanged.
    let node_descriptor = rms::NodeDescriptor {
        attributes: profile
            .attributes
            .iter()
            .chain(role_attributes)
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .chain([
                (KEY_ROLE.to_string(), role.descriptor_value().to_string()),
                (KEY_VENDOR.to_string(), vendor.to_string()),
                (KEY_PRODUCT_FAMILY.to_string(), product_family.to_string()),
            ])
            .collect(),
    };

    let legacy_node_type = legacy_node_type(role, product_family, vendor);

    tracing::debug!(
        role = role.descriptor_value(),
        vendor,
        product_family,
        ?legacy_node_type,
        "Built RMS node identity"
    );

    Ok(RmsNodeIdentity {
        role,
        legacy_node_type,
        node_descriptor,
    })
}

/// Resolves an enum override for RMS servers that predate descriptor dispatch.
///
/// Unsupported combinations intentionally return `None`; this mapping must
/// never reject a rack profile or override descriptor-based dispatch.
fn legacy_node_type(
    role: RmsNodeRole,
    product_family: &str,
    vendor: &str,
) -> Option<rms::NodeType> {
    let product_family = normalize_descriptor_value(product_family);
    let vendor = normalize_descriptor_value(vendor);

    match (role, product_family.as_str(), vendor.as_str()) {
        (RmsNodeRole::Compute, "gb200", "nvidia") => Some(rms::NodeType::ComputeGb200Nvidia),
        (RmsNodeRole::Compute, "gb300", "nvidia") => Some(rms::NodeType::ComputeGb300Nvidia),
        (RmsNodeRole::Compute, "gb300", "lenovo") => Some(rms::NodeType::ComputeGb300Lenovo),
        (RmsNodeRole::Compute, "vrnvl72", "nvidia") => Some(rms::NodeType::ComputeVrnvl72Nvidia),
        (RmsNodeRole::Switch, "gb200", "nvidia") => Some(rms::NodeType::SwitchGb200Nvidia),
        (RmsNodeRole::Switch, "gb300", "nvidia") => Some(rms::NodeType::SwitchGb300Nvidia),
        (RmsNodeRole::Switch, "vrnvl72", "nvidia") => Some(rms::NodeType::SwitchVrnvl72Nvidia),
        (RmsNodeRole::PowerShelf, "gb200", "liteon") => Some(rms::NodeType::PowershelfGb200Liteon),
        (RmsNodeRole::PowerShelf, "gb200", "delta") => Some(rms::NodeType::PowershelfGb200Delta),
        (RmsNodeRole::PowerShelf, "gb300", "liteon") => Some(rms::NodeType::PowershelfGb300Liteon),
        (RmsNodeRole::PowerShelf, "gb300", "delta") => Some(rms::NodeType::PowershelfGb300Delta),
        _ => None,
    }
}

// Keep the named vendor and custom attributes paired so validation, warning,
// and descriptor construction all resolve the same capability block.
fn vendor_and_attributes_for_role(
    profile: &RackProfile,
    role: RmsNodeRole,
) -> (Option<&str>, &HashMap<String, String>) {
    match role {
        RmsNodeRole::Compute => (
            profile.rack_capabilities.compute.vendor.as_deref(),
            &profile.rack_capabilities.compute.attributes,
        ),
        RmsNodeRole::Switch => (
            profile.rack_capabilities.switch.vendor.as_deref(),
            &profile.rack_capabilities.switch.attributes,
        ),
        RmsNodeRole::PowerShelf => (
            profile.rack_capabilities.power_shelf.vendor.as_deref(),
            &profile.rack_capabilities.power_shelf.attributes,
        ),
    }
}

fn normalize_descriptor_value(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-', '_'], "")
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::capture_logs;
    use model::rack_type::{RackHardwareType, RackProductFamily};

    use super::*;

    fn profile_with_product_family(product_family: RackProductFamily) -> RackProfile {
        RackProfile {
            product_family: Some(product_family),
            ..Default::default()
        }
    }

    #[test]
    fn vendor_matching_trims_outer_whitespace() {
        let mut profile = profile_with_product_family(RackProductFamily::Gb200);
        profile.rack_capabilities.compute.vendor = Some("\tNVIDIA\n".to_string());

        let identity = compute_node_identity_for_profile(&profile).unwrap();

        assert_eq!(
            identity.legacy_node_type,
            Some(rms::NodeType::ComputeGb200Nvidia)
        );

        assert_eq!(
            identity.node_descriptor.attributes.get(KEY_VENDOR),
            Some(&"NVIDIA".to_string())
        );
    }

    #[test]
    fn arbitrary_product_family_and_vendor_are_descriptor_data() {
        let mut profile = profile_with_product_family(RackProductFamily::Other(
            " test-product-family ".to_string(),
        ));

        profile.rack_capabilities.compute.vendor = Some("test-compute-vendor".to_string());

        let identity = compute_node_identity_for_profile(&profile).unwrap();

        assert_eq!(identity.legacy_node_type, None);

        assert_eq!(
            identity.node_descriptor.attributes.get(KEY_VENDOR),
            Some(&"test-compute-vendor".to_string())
        );

        assert_eq!(
            identity.node_descriptor.attributes.get(KEY_PRODUCT_FAMILY),
            Some(&"test-product-family".to_string())
        );
    }

    #[test]
    fn compute_identity_uses_descriptor() {
        let mut profile = profile_with_product_family(RackProductFamily::Gb200);
        profile.rack_capabilities.compute.vendor = Some("NVIDIA".to_string());

        let identity = compute_node_identity_for_profile(&profile).unwrap();

        assert_eq!(identity.role, RmsNodeRole::Compute);

        assert_eq!(
            identity.legacy_node_type,
            Some(rms::NodeType::ComputeGb200Nvidia)
        );

        assert_eq!(
            identity.node_descriptor.attributes.get(KEY_ROLE),
            Some(&ROLE_COMPUTE.to_string())
        );

        assert_eq!(
            identity.node_descriptor.attributes.get(KEY_VENDOR),
            Some(&"NVIDIA".to_string())
        );

        assert_eq!(
            identity.node_descriptor.attributes.get(KEY_PRODUCT_FAMILY),
            Some(&"gb200".to_string())
        );

        assert_eq!(identity.node_descriptor.attributes.len(), 3);
    }

    #[test]
    fn switch_identity_uses_descriptor() {
        let mut profile = profile_with_product_family(RackProductFamily::Gb300);
        profile.rack_capabilities.switch.vendor = Some("test-switch-vendor".to_string());

        let identity = switch_node_identity_for_profile(&profile).unwrap();

        assert_eq!(identity.role, RmsNodeRole::Switch);

        assert_eq!(
            identity.node_descriptor.attributes.get(KEY_ROLE),
            Some(&ROLE_SWITCH.to_string())
        );

        assert_eq!(
            identity.node_descriptor.attributes.get(KEY_VENDOR),
            Some(&"test-switch-vendor".to_string())
        );

        assert_eq!(
            identity.node_descriptor.attributes.get(KEY_PRODUCT_FAMILY),
            Some(&"gb300".to_string())
        );
    }

    #[test]
    fn descriptor_attributes_follow_inheritance_precedence() {
        type IdentityBuilder = fn(&RackProfile) -> Result<RmsNodeIdentity, NodeDescriptorError>;

        let mut profile = profile_with_product_family(RackProductFamily::Gb300);

        profile.attributes = HashMap::from([
            (
                "additional_attribute2".to_string(),
                "  MiXeD-Value  ".to_string(),
            ),
            ("attribute1".to_string(), "rack-value".to_string()),
            ("role".to_string(), "custom-role".to_string()),
            ("vendor".to_string(), "custom-vendor".to_string()),
            (
                "product_family".to_string(),
                "custom-product-family".to_string(),
            ),
        ]);

        profile.rack_capabilities.compute.vendor = Some("NVIDIA".to_string());

        profile.rack_capabilities.compute.attributes = HashMap::from([
            ("attribute1".to_string(), "compute-value".to_string()),
            ("role".to_string(), "custom-compute-role".to_string()),
            ("vendor".to_string(), "custom-compute-vendor".to_string()),
            (
                "product_family".to_string(),
                "custom-compute-family".to_string(),
            ),
        ]);

        profile.rack_capabilities.switch.vendor = Some("NVIDIA".to_string());

        profile.rack_capabilities.switch.attributes = HashMap::from([
            ("attribute1".to_string(), "switch-value".to_string()),
            ("role".to_string(), "custom-switch-role".to_string()),
            ("vendor".to_string(), "custom-switch-vendor".to_string()),
            (
                "product_family".to_string(),
                "custom-switch-family".to_string(),
            ),
        ]);

        profile.rack_capabilities.power_shelf.vendor = Some("Delta".to_string());

        profile.rack_capabilities.power_shelf.attributes = HashMap::from([
            ("attribute1".to_string(), "power-shelf-value".to_string()),
            ("role".to_string(), "custom-power-shelf-role".to_string()),
            (
                "vendor".to_string(),
                "custom-power-shelf-vendor".to_string(),
            ),
            (
                "product_family".to_string(),
                "custom-power-shelf-family".to_string(),
            ),
        ]);

        let cases: [(&str, &str, &str, IdentityBuilder); 3] = [
            (
                ROLE_COMPUTE,
                "NVIDIA",
                "compute-value",
                compute_node_identity_for_profile,
            ),
            (
                ROLE_SWITCH,
                "NVIDIA",
                "switch-value",
                switch_node_identity_for_profile,
            ),
            (
                ROLE_POWER_SHELF,
                "Delta",
                "power-shelf-value",
                power_shelf_node_identity_for_profile,
            ),
        ];

        for (expected_role, expected_vendor, expected_attribute1, build_identity) in cases {
            let attributes = build_identity(&profile).unwrap().node_descriptor.attributes;

            assert_eq!(
                attributes.get("additional_attribute2").map(String::as_str),
                Some("  MiXeD-Value  ")
            );

            assert_eq!(
                attributes.get("attribute1").map(String::as_str),
                Some(expected_attribute1)
            );

            assert_eq!(
                attributes.get(KEY_ROLE).map(String::as_str),
                Some(expected_role)
            );

            assert_eq!(
                attributes.get(KEY_VENDOR).map(String::as_str),
                Some(expected_vendor)
            );

            assert_eq!(
                attributes.get(KEY_PRODUCT_FAMILY).map(String::as_str),
                Some("gb300")
            );
        }
    }

    #[test]
    fn attribute_collisions_emit_non_fatal_structured_warnings() {
        let mut profile = RackProfile::default();
        profile
            .attributes
            .insert("attribute1".to_string(), "rack-value".to_string());

        profile
            .rack_capabilities
            .compute
            .attributes
            .extend(HashMap::from([
                ("attribute1".to_string(), "compute-value".to_string()),
                ("vendor".to_string(), "custom-vendor".to_string()),
            ]));

        profile.rack_capabilities.compute.vendor = Some("NVIDIA".to_string());

        let config = RackProfileConfig {
            rack_profiles: HashMap::from([("NVL72".to_string(), profile)]),
        };

        let logs = capture_logs(|| warn_rms_node_descriptor_attribute_overrides(&config));

        assert_eq!(logs.len(), 2);

        assert!(logs.iter().all(|log| {
            log.level == tracing::Level::WARN
                && log.message == "RMS node descriptor attribute is overridden"
                && log.field("rack_profile_id") == Some("NVL72")
                && log.field("role") == Some(ROLE_COMPUTE)
        }));

        assert!(logs.iter().any(|log| {
            log.field("attribute_key") == Some("attribute1")
                && log.field("overridden_source") == Some("rack")
                && log.field("winning_source") == Some("role")
        }));

        assert!(logs.iter().any(|log| {
            log.field("attribute_key") == Some(KEY_VENDOR)
                && log.field("overridden_source") == Some("role")
                && log.field("winning_source") == Some("named_field")
        }));
    }

    #[test]
    fn missing_named_fields_do_not_emit_override_warnings() {
        let mut profile = RackProfile::default();

        profile.attributes.insert(
            KEY_PRODUCT_FAMILY.to_string(),
            "custom-product-family".to_string(),
        );

        profile
            .rack_capabilities
            .compute
            .attributes
            .insert(KEY_VENDOR.to_string(), "custom-vendor".to_string());

        let config = RackProfileConfig {
            rack_profiles: HashMap::from([("NVL72".to_string(), profile)]),
        };

        let logs = capture_logs(|| warn_rms_node_descriptor_attribute_overrides(&config));

        assert!(logs.is_empty(), "{logs:?}");
    }

    #[test]
    fn apply_to_node_info_sets_descriptor_and_legacy_node_type() {
        let mut profile = profile_with_product_family(RackProductFamily::Gb300);
        profile.rack_capabilities.power_shelf.vendor = Some("Delta".to_string());
        let identity = power_shelf_node_identity_for_profile(&profile).unwrap();

        let mut node = rms::NodeInfo {
            r#type: Some(1),
            ..Default::default()
        };

        identity.apply_to_node_info(&mut node);

        assert_eq!(
            node.r#type,
            Some(rms::NodeType::PowershelfGb300Delta as i32)
        );

        assert_eq!(node.node_descriptor, Some(identity.node_descriptor));
    }

    #[test]
    fn component_filters_include_descriptor_and_legacy_node_type() {
        let mut profile = profile_with_product_family(RackProductFamily::Gb300);
        profile.rack_capabilities.switch.vendor = Some("NVIDIA".to_string());
        let identity = switch_node_identity_for_profile(&profile).unwrap();

        let (component_filters, descriptor_filters) =
            firmware_object_component_filters_for_node_identities(
                &["BMC".to_string()],
                [&identity],
            );

        assert_eq!(component_filters.len(), 1);
        assert_eq!(descriptor_filters.len(), 1);

        assert_eq!(
            component_filters
                .get(&(rms::NodeType::SwitchGb300Nvidia as i32))
                .map(|filter| filter.components.as_slice()),
            Some(["BMC".to_string()].as_slice())
        );

        assert_eq!(
            descriptor_filters[0]
                .component_filter
                .as_ref()
                .map(|filter| filter.components.as_slice()),
            Some(["BMC".to_string()].as_slice())
        );
    }

    #[test]
    fn vrnvl72_power_shelf_uses_descriptor_without_legacy_node_type() {
        let mut profile =
            profile_with_product_family(RackProductFamily::Other("vrnvl72".to_string()));

        profile.rack_capabilities.power_shelf.vendor = Some("Delta".to_string());

        let identity = power_shelf_node_identity_for_profile(&profile).unwrap();

        assert_eq!(identity.legacy_node_type, None);

        let mut node = rms::NodeInfo::default();
        identity.apply_to_node_info(&mut node);

        assert_eq!(node.r#type, None);

        assert_eq!(
            node.node_descriptor.as_ref(),
            Some(&identity.node_descriptor)
        );

        let (component_filters, descriptor_filters) =
            firmware_object_component_filters_for_node_identities(
                &["BMC".to_string()],
                [&identity],
            );

        assert!(component_filters.is_empty());
        assert_eq!(descriptor_filters.len(), 1);
    }

    #[test]
    fn legacy_node_type_matches_exact_supported_matrix() {
        let cases = [
            (
                RmsNodeRole::Compute,
                "gb200",
                "NVIDIA",
                Some(rms::NodeType::ComputeGb200Nvidia),
            ),
            (
                RmsNodeRole::Compute,
                "gb300",
                "NVIDIA",
                Some(rms::NodeType::ComputeGb300Nvidia),
            ),
            (
                RmsNodeRole::Compute,
                "gb300",
                "Lenovo",
                Some(rms::NodeType::ComputeGb300Lenovo),
            ),
            (
                RmsNodeRole::Compute,
                "vr_nvl72",
                "NVIDIA",
                Some(rms::NodeType::ComputeVrnvl72Nvidia),
            ),
            (
                RmsNodeRole::Switch,
                "gb200",
                "NVIDIA",
                Some(rms::NodeType::SwitchGb200Nvidia),
            ),
            (
                RmsNodeRole::Switch,
                "gb300",
                "NVIDIA",
                Some(rms::NodeType::SwitchGb300Nvidia),
            ),
            (
                RmsNodeRole::Switch,
                "vr_nvl72",
                "NVIDIA",
                Some(rms::NodeType::SwitchVrnvl72Nvidia),
            ),
            (
                RmsNodeRole::PowerShelf,
                "gb200",
                "LiteOn",
                Some(rms::NodeType::PowershelfGb200Liteon),
            ),
            (
                RmsNodeRole::PowerShelf,
                "gb200",
                "Delta",
                Some(rms::NodeType::PowershelfGb200Delta),
            ),
            (
                RmsNodeRole::PowerShelf,
                "gb300",
                "LiteOn",
                Some(rms::NodeType::PowershelfGb300Liteon),
            ),
            (
                RmsNodeRole::PowerShelf,
                "gb300",
                "Delta",
                Some(rms::NodeType::PowershelfGb300Delta),
            ),
            (RmsNodeRole::PowerShelf, "vrnvl72", "Delta", None),
            (RmsNodeRole::Compute, "vrnvl144", "NVIDIA", None),
            (RmsNodeRole::Compute, "gb200", "NVIDIA Corp", None),
        ];

        for (role, product_family, vendor, expected) in cases {
            assert_eq!(
                legacy_node_type(role, product_family, vendor),
                expected,
                "role={role:?}, product_family={product_family}, vendor={vendor}"
            );
        }
    }

    #[test]
    fn identity_requires_product_family_even_with_hardware_type() {
        let mut profile = RackProfile {
            rack_hardware_type: Some(RackHardwareType("test-hardware-type".to_string())),
            ..Default::default()
        };

        profile.rack_capabilities.compute.vendor = Some("test-compute-vendor".to_string());

        profile.attributes.insert(
            KEY_PRODUCT_FAMILY.to_string(),
            "custom-product-family".to_string(),
        );

        let err = compute_node_identity_for_profile(&profile);

        assert_eq!(err, Err(NodeDescriptorError::MissingProductFamily));
    }

    #[test]
    fn identity_rejects_blank_programmatic_product_family() {
        let mut profile = profile_with_product_family(RackProductFamily::Other(" \t ".to_string()));
        profile.rack_capabilities.compute.vendor = Some("NVIDIA".to_string());

        let err = compute_node_identity_for_profile(&profile);

        assert_eq!(err, Err(NodeDescriptorError::MissingProductFamily));
    }

    #[test]
    fn identity_requires_role_vendor() {
        let mut profile = profile_with_product_family(RackProductFamily::Gb200);
        profile
            .rack_capabilities
            .power_shelf
            .attributes
            .insert(KEY_VENDOR.to_string(), "custom-vendor".to_string());

        let err = power_shelf_node_identity_for_profile(&profile);

        assert_eq!(
            err,
            Err(NodeDescriptorError::VendorMissing {
                role: "power shelf",
            })
        );
    }
}
