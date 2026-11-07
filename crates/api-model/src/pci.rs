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

//! Numeric ordering keys for UEFI PCI paths and PCI BDFs.
//!
//! UEFI paths and Linux BDFs use different root identifier domains. Distinct
//! wrapper types prevent direct cross-format comparison while sharing numeric
//! component ordering.

use std::num::ParseIntError;

use lazy_static::lazy_static;
use regex::Regex;

const LEGACY_UEFI_ORDERING_COMPONENT_MAX: u32 = i32::MAX.unsigned_abs();

/// The shared numeric, lexicographically ordered representation of a PCI key.
///
/// Canonicalizing trailing zeros preserves Site Explorer's legacy
/// `version_compare` equality. PCI BDFs always supply four components, so the
/// same canonicalization does not change their relative ordering.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct NumericPciOrderingKey(Vec<u32>);

impl NumericPciOrderingKey {
    fn new(mut components: Vec<u32>) -> Self {
        while components.len() > 1 && components.last() == Some(&0) {
            components.pop();
        }
        Self(components)
    }
}

/// A UEFI PCI topology ordering key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct UefiPciOrderingKey(NumericPciOrderingKey);

/// A numeric PCI domain/bus/device/function ordering key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PciBdfOrderingKey(NumericPciOrderingKey);

/// Error returned when a PCI BDF cannot form an ordering key.
#[derive(Debug, thiserror::Error)]
#[error("malformed or out-of-range PCI BDF `{bdf}`")]
pub struct PciBdfParseError {
    bdf: String,
}

/// Error returned when UEFI PCI data cannot form an ordering key.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UefiPciOrderingKeyParseError {
    #[error("missing UEFI PciRoot node")]
    MissingUefiRoot,
    #[error("missing pci node after UEFI PciRoot")]
    MissingUefiHop,
    #[error("invalid hexadecimal PCI component `{component}`")]
    InvalidHexComponent {
        component: String,
        #[source]
        source: ParseIntError,
    },
    #[error("invalid decimal PCI component `{component}`")]
    InvalidDecimalComponent {
        component: String,
        #[source]
        source: ParseIntError,
    },
    #[error("normalized UEFI PCI component `{component}` exceeds the supported ordering range")]
    OrderingComponentOutOfRange { component: String },
}

lazy_static! {
    // Not anchored at start: GB300/Grace UEFI device paths can prefix PciRoot
    // with vendor and memory-mapped nodes.
    static ref PCI_ROOT_REGEX: Regex =
        Regex::new(r"PciRoot\(([^)]*)\)").expect("must always compile");
    static ref PCI_NODE_REGEX: Regex =
        Regex::new(r"/Pci\(([^)]*)\)").expect("must always compile");
}

impl UefiPciOrderingKey {
    /// Parses the persisted dotted-decimal representation of a UEFI path.
    pub(crate) fn from_normalized_uefi_path(
        path: &str,
    ) -> Result<Self, UefiPciOrderingKeyParseError> {
        if path.is_empty() {
            return Err(UefiPciOrderingKeyParseError::MissingUefiRoot);
        }
        let components = path
            .split('.')
            .map(|component| {
                let value = component.parse::<u32>().map_err(|source| {
                    UefiPciOrderingKeyParseError::InvalidDecimalComponent {
                        component: component.to_string(),
                        source,
                    }
                })?;
                if value > LEGACY_UEFI_ORDERING_COMPONENT_MAX {
                    return Err(UefiPciOrderingKeyParseError::OrderingComponentOutOfRange {
                        component: component.to_string(),
                    });
                }
                Ok(value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if components.len() == 1 {
            return Err(UefiPciOrderingKeyParseError::MissingUefiHop);
        }

        Ok(Self(NumericPciOrderingKey::new(components)))
    }
}

impl PciBdfOrderingKey {
    /// Parses a PCI BDF in `domain:bus:device.function` form.
    ///
    /// Components are variable-width hexadecimal values. Surrounding whitespace is ignored;
    /// domain, bus, device, and function are bounded to 16, 8, 5, and 3 bits respectively.
    ///
    /// One audited GB200 report, for example, pairs UEFI
    /// `PciRoot(0x6)/Pci(0x0,0x0)/Pci(0x0,0x0)` with scout `DEVPATH`
    /// `/devices/pci0006:00/0006:00:00.0/0006:01:00.0/net/en0` and slot
    /// `0006:01:00.0`. The equivalent root-16 UEFI and BDF records sort after
    /// the root-6 records.
    /// `DEVPATH` root identity and hop shape are not portable across vendors,
    /// so scout ordering deliberately parses the numeric endpoint slot instead.
    pub fn from_bdf(bdf: &str) -> Result<Self, PciBdfParseError> {
        let bdf = bdf.trim();
        let malformed = || PciBdfParseError {
            bdf: bdf.to_string(),
        };

        let mut colon_components = bdf.split(':');
        let domain = colon_components.next().ok_or_else(&malformed)?;
        let bus = colon_components.next().ok_or_else(&malformed)?;
        let endpoint = colon_components.next().ok_or_else(&malformed)?;
        if colon_components.next().is_some() {
            return Err(malformed());
        }

        let mut endpoint_components = endpoint.split('.');
        let device = endpoint_components.next().ok_or_else(&malformed)?;
        let function = endpoint_components.next().ok_or_else(&malformed)?;
        if endpoint_components.next().is_some() {
            return Err(malformed());
        }

        let components = [
            parse_bdf_component(domain, 0xffff).ok_or_else(&malformed)?,
            parse_bdf_component(bus, 0xff).ok_or_else(&malformed)?,
            parse_bdf_component(device, 0x1f).ok_or_else(&malformed)?,
            parse_bdf_component(function, 0x7).ok_or_else(&malformed)?,
        ];
        Ok(Self(NumericPciOrderingKey::new(components.to_vec())))
    }
}

/// Returns the existing dotted-decimal serialization of a UEFI PCI path.
pub(crate) fn normalize_uefi_device_path(
    path: &str,
) -> Result<String, UefiPciOrderingKeyParseError> {
    parse_uefi_components(path).map(|components| {
        components
            .into_iter()
            .map(|component| component.to_string())
            .collect::<Vec<_>>()
            .join(".")
    })
}

/// Parses UEFI root and hop components while preserving their existing order.
fn parse_uefi_components(path: &str) -> Result<Vec<u32>, UefiPciOrderingKeyParseError> {
    // UEFI 2.10 section 10.3.2.1 describes PciRoot followed by one or more Pci
    // nodes. A trailing MAC node is not part of the PCI topology.
    let path_without_mac = path.rsplit_once("/MAC").map_or(path, |(path, _)| path);
    let root = PCI_ROOT_REGEX
        .captures(path_without_mac)
        .and_then(|captures| captures.get(1))
        .ok_or(UefiPciOrderingKeyParseError::MissingUefiRoot)?;

    let mut components = parse_uefi_group(root.as_str())?;
    let mut had_pci = false;
    for captures in PCI_NODE_REGEX.captures_iter(path_without_mac) {
        if let Some(group) = captures.get(1) {
            had_pci = true;
            components.extend(parse_uefi_group(group.as_str())?);
        }
    }
    if !had_pci {
        return Err(UefiPciOrderingKeyParseError::MissingUefiHop);
    }

    Ok(components)
}

fn parse_uefi_group(group: &str) -> Result<Vec<u32>, UefiPciOrderingKeyParseError> {
    group
        .split(',')
        .map(|component| {
            let hexadecimal = component
                .strip_prefix("0x")
                .or_else(|| component.strip_prefix("0X"))
                .unwrap_or(component);
            u32::from_str_radix(hexadecimal, 16).map_err(|source| {
                UefiPciOrderingKeyParseError::InvalidHexComponent {
                    component: component.to_string(),
                    source,
                }
            })
        })
        .collect()
}

fn parse_bdf_component(component: &str, maximum: u32) -> Option<u32> {
    if component.is_empty()
        || !component
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return None;
    }
    let value = u32::from_str_radix(component, 16).ok()?;
    (value <= maximum).then_some(value)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn audited_gb200_uefi_and_bdf_examples_have_the_same_order() {
        // Stored reports validate this source-local ordering for one GB200 topology:
        // UEFI PciRoot(0x6)/Pci(0,0)/Pci(0,0)
        // scout DEVPATH /devices/pci0006:00/0006:00:00.0/0006:01:00.0/net/en0,
        // slot 0006:01:00.0; and equivalent root/domain 0x16 records. This does
        // not claim that UEFI root UIDs and scout BDF domains match universally.
        let uefi_key = |path| {
            UefiPciOrderingKey::from_normalized_uefi_path(
                &normalize_uefi_device_path(path).unwrap(),
            )
            .unwrap()
        };
        let uefi_six = uefi_key("PciRoot(0x6)/Pci(0x0,0x0)/Pci(0x0,0x0)");
        let uefi_sixteen = uefi_key("PciRoot(0x16)/Pci(0x0,0x0)/Pci(0x0,0x0)");
        let scout_six = PciBdfOrderingKey::from_bdf("0006:01:00.0").unwrap();
        let scout_sixteen = PciBdfOrderingKey::from_bdf("0016:01:00.0").unwrap();

        assert!(uefi_six < uefi_sixteen);
        assert!(scout_six < scout_sixteen);
    }

    #[test]
    fn scout_bdf_is_numeric_and_normalized() {
        let two = PciBdfOrderingKey::from_bdf("0:2:0.0").unwrap();
        let ten = PciBdfOrderingKey::from_bdf("0:10:0.0").unwrap();
        let padded_upper = PciBdfOrderingKey::from_bdf(" 000A:000B:000C.07 ").unwrap();
        let compact_lower = PciBdfOrderingKey::from_bdf("a:b:c.7").unwrap();

        assert!(two < ten);
        assert_eq!(padded_upper, compact_lower);
    }

    #[test]
    fn scout_bdf_requires_complete_bounded_components() {
        scenarios!(run = |bdf| PciBdfOrderingKey::from_bdf(bdf).map(drop).map_err(drop);
            "complete variable-width BDF" {
                "0:2:0.0" => Yields(()),
            }
            "missing function" {
                "0:2:0" => Fails,
            }
            "extra text" {
                "0:2:0.0/net" => Fails,
            }
            "domain exceeds range" {
                "10000:2:0.0" => Fails,
            }
            "bus exceeds range" {
                "0:100:0.0" => Fails,
            }
            "device exceeds range" {
                "0:2:20.0" => Fails,
            }
            "function exceeds range" {
                "0:2:0.8" => Fails,
            }
        );
    }

    #[test]
    fn uefi_ordering_fails_closed_outside_the_legacy_numeric_range() {
        let supported = format!("{LEGACY_UEFI_ORDERING_COMPONENT_MAX}.0.0");
        let unsupported = format!("{}.0.0", LEGACY_UEFI_ORDERING_COMPONENT_MAX + 1);

        assert!(UefiPciOrderingKey::from_normalized_uefi_path(&supported).is_ok());
        assert!(matches!(
            UefiPciOrderingKey::from_normalized_uefi_path(&unsupported),
            Err(UefiPciOrderingKeyParseError::OrderingComponentOutOfRange { .. })
        ));
    }

    #[test]
    fn shared_key_canonicalization_preserves_legacy_uefi_ties() {
        assert_eq!(
            NumericPciOrderingKey::new(vec![7, 2]),
            NumericPciOrderingKey::new(vec![7, 2, 0, 0]),
        );

        let direct = UefiPciOrderingKey::from_normalized_uefi_path("7.2.0").unwrap();
        let behind_zero_hop = UefiPciOrderingKey::from_normalized_uefi_path("7.2.0.0.0").unwrap();
        assert_eq!(direct, behind_zero_hop);
    }

    #[test]
    fn uefi_key_retains_nonzero_topology_depth() {
        let direct = UefiPciOrderingKey::from_normalized_uefi_path("7.2.0").unwrap();
        let behind_bridge = UefiPciOrderingKey::from_normalized_uefi_path("7.2.0.1.0").unwrap();

        assert!(direct < behind_bridge);
    }
}
