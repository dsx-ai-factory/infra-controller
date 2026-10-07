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
use carbide_utils::none_if_empty::NoneIfEmpty;
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

/// Which selection mechanism established the machine's current boot interface.
///
/// A source records the path NICo used to make the decision, not necessarily a
/// unique hardware discriminator. This mirrors the
/// `boot_interface_selection_source` Postgres enum. Sources that come from
/// Redfish name the exact Redfish signal used, while [`Self::LegacyUnknown`]
/// covers selections that predate tracking or whose current source cannot be
/// recovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(
    type_name = "boot_interface_selection_source",
    rename_all = "snake_case"
)]
#[serde(rename_all = "snake_case")]
pub enum BootInterfaceSelectionSource {
    /// An `ExpectedMachine` explicitly declared the primary interface.
    ExpectedMachine,
    /// An operator explicitly selected the boot interface.
    Operator,
    /// Redfish UEFI PCI paths determined the ordering.
    RedfishUefiPci,
    /// Redfish chassis identifiers determined the ordering.
    RedfishChassisId,
    /// Site Explorer used its DPU serial number fallback from Redfish.
    /// Missing serials sort as empty keys, and the stable sort retains discovery
    /// order among equal keys.
    RedfishSerialNumber,
    /// Host PCI data in a Scout report determined the ordering.
    ScoutReportPci,
    /// The selection predates source tracking or otherwise has no known source.
    LegacyUnknown,
}

/// Whether forced boot interface reconciliation selects a target or reapplies
/// the target that was already selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootInterfaceSelectionAuthority {
    /// An operator explicitly selected the target being reconciled.
    Operator,
    /// Reconciliation preserves existing selection metadata, or records
    /// `LegacyUnknown` when no prior selection exists.
    Existing,
}

/// Persisted explanation for the current desired boot interface selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootInterfaceSelection {
    /// Mechanism that established the current selection.
    pub source: BootInterfaceSelectionSource,
    /// Time the current target and source decision was established.
    ///
    /// Adding a Redfish interface ID for the same MAC preserves this time. It is
    /// absent when NICo cannot recover the decision time, including migrated
    /// rows and compatibility baselines. It is distinct from Redfish
    /// observation time, which records convergence.
    pub updated_at: Option<DateTime<Utc>>,
}

/// Returns the canonical form of a Redfish boot-interface id.
///
/// Redfish ids may arrive padded by transport or vendor formatting. Only
/// boundary ASCII whitespace is removed so Rust and the database apply the
/// same rule without changing any non-ASCII identifier characters. An empty
/// canonical value is not a usable id.
pub fn canonical_redfish_boot_interface_id(interface_id: &str) -> Option<&str> {
    let interface_id = interface_id.trim_matches(|character: char| {
        matches!(character, ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r')
    });
    (!interface_id.is_empty()).then_some(interface_id)
}

/// A host's boot interface, identified by *both* its MAC address and its
/// vendor-native Redfish `EthernetInterface.Id`.
///
/// Both fields are always present: a `MachineBootInterface` is only ever
/// constructed from a fully-populated pair, captured while the MAC was still
/// reported by Redfish. When this complete pair is available, boot-interface
/// callers pass both identifiers to `libredfish` as one `BootInterfaceRef::Pair`;
/// callers without an `interface_id` target the MAC alone. This allows each
/// vendor to use the identifier its implementation expects. Dell uses
/// `interface_id` directly, which keeps the boot interface addressable after a
/// BlueField operating-mode flip removes its MAC from `NetworkDeviceFunctions`,
/// `EthernetInterfaces`, and `NetworkAdapters`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MachineBootInterface {
    /// MAC address of the boot interface.
    pub mac_address: MacAddress,
    /// Vendor-native Redfish `EthernetInterface.Id` of the boot interface
    /// (e.g. `"NIC.Slot.7-1-1"`).
    pub interface_id: String,
}

impl MachineBootInterface {
    /// Builds a `MachineBootInterface` from the optional `(mac, interface_id)`
    /// parts of a record, returning `Some` only when *both* are present and the
    /// interface id is non-empty.
    ///
    /// This is the single place the "only retain a fully-populated boot
    /// interface" rule is enforced: a partial pair (a missing MAC, or a missing
    /// or empty interface id) yields `None`, so callers keep the last-known-good
    /// record rather than persisting a half-empty one.
    pub fn from_parts(
        mac_address: Option<MacAddress>,
        interface_id: Option<String>,
    ) -> Option<Self> {
        Some(Self {
            mac_address: mac_address?,
            interface_id: interface_id.none_if_empty()?,
        })
    }

    /// [`Self::from_parts`] for records whose MAC is always present (interface
    /// rows, predictions): only the interface id can be missing.
    pub fn for_mac(mac_address: MacAddress, interface_id: Option<String>) -> Option<Self> {
        Self::from_parts(Some(mac_address), interface_id)
    }
}

/// A host boot-interface target used for both desired configuration and
/// Redfish observations.
///
/// NICo retains the complete [`MachineBootInterface`] pair whenever both
/// identifiers are known. Older records and newly discovered targets may only
/// have the MAC, which remains a valid selector. A backend may match with only the
/// identifiers its read path supports -- NvRedfish currently uses the MAC --
/// while this value keeps the `Pair` identity NICo requested. The state
/// controller can therefore compare the logical target with its current target
/// before trusting the associated setup status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum MachineBootInterfaceTarget {
    /// Both the MAC and vendor-native Redfish interface id are known.
    Pair(MachineBootInterface),
    /// Only the MAC is known.
    MacOnly(MacAddress),
}

/// Status for the desired boot-interface generation currently treated as converged.
///
/// `assumed` is true for the compatibility baseline used when an already-stable
/// host has no persisted row, including during mixed-component rollout. Real
/// Redfish verification always records it as false.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootInterfaceStatusObservation {
    /// Desired boot-interface configuration version this status applies to.
    pub config_version: ConfigVersion,
    /// Time this status was recorded.
    pub observed_at: DateTime<Utc>,
    /// Whether this is a compatibility baseline rather than a Redfish observation.
    pub assumed: bool,
}

impl MachineBootInterfaceTarget {
    /// Builds the strongest usable target from an endpoint record.
    ///
    /// A MAC plus a non-empty interface id becomes [`Self::Pair`]. A MAC
    /// without an id remains [`Self::MacOnly`], while an id without a MAC
    /// cannot identify an interface and yields `None`.
    pub fn from_parts(
        mac_address: Option<MacAddress>,
        interface_id: Option<String>,
    ) -> Option<Self> {
        let mac_address = mac_address?;
        Some(match interface_id.none_if_empty() {
            Some(interface_id) => Self::Pair(MachineBootInterface {
                mac_address,
                interface_id,
            }),
            None => Self::MacOnly(mac_address),
        })
    }

    /// Returns the MAC address used to identify this target.
    pub fn mac_address(&self) -> MacAddress {
        match self {
            Self::Pair(interface) => interface.mac_address,
            Self::MacOnly(mac_address) => *mac_address,
        }
    }

    /// Returns the vendor-native Redfish interface id when it is known.
    pub fn interface_id(&self) -> Option<&str> {
        match self {
            Self::Pair(interface) => Some(&interface.interface_id),
            Self::MacOnly(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn redfish_boot_interface_ids_use_ascii_boundary_whitespace() {
        value_scenarios!(run = |interface_id| {
            canonical_redfish_boot_interface_id(interface_id)
        };
            "canonical" {
                "NIC.Slot.7-1-1" => Some("NIC.Slot.7-1-1"),
            }

            "padded valid id" {
                " \t\n\u{000b}\u{000c}\rNIC.Slot.7-1-1 \t\n\u{000b}\u{000c}\r" =>
                    Some("NIC.Slot.7-1-1"),
            }

            "ASCII whitespace only" {
                "\t\n" => None,
            }

            "non-ASCII boundary whitespace is retained" {
                "\u{00a0}NIC.Slot.7-1-1\u{00a0}" =>
                    Some("\u{00a0}NIC.Slot.7-1-1\u{00a0}"),
            }
        );
    }

    #[test]
    fn from_parts_requires_both() {
        let mac = MacAddress::new([1, 2, 3, 4, 5, 6]);

        assert_eq!(
            MachineBootInterface::from_parts(Some(mac), Some("NIC.Slot.7-1-1".to_string())),
            Some(MachineBootInterface {
                mac_address: mac,
                interface_id: "NIC.Slot.7-1-1".to_string(),
            })
        );
        assert_eq!(
            MachineBootInterface::from_parts(Some(mac), None),
            None,
            "a present MAC with no interface id is not fully populated"
        );
        assert_eq!(
            MachineBootInterface::from_parts(Some(mac), Some(String::new())),
            None,
            "a present MAC with an empty interface id is not fully populated"
        );
        assert_eq!(
            MachineBootInterface::from_parts(None, Some("NIC.Slot.7-1-1".to_string())),
            None,
            "an interface id with no MAC is not fully populated"
        );
        assert_eq!(MachineBootInterface::from_parts(None, None), None);
    }

    #[test]
    fn target_from_parts_preserves_the_available_selector() {
        let mac = MacAddress::new([1, 2, 3, 4, 5, 6]);

        value_scenarios!(run = |(mac_address, interface_id)| {
            MachineBootInterfaceTarget::from_parts(mac_address, interface_id)
        };
            "complete pair" {
                (Some(mac), Some("NIC.Slot.7-1-1".to_string())) =>
                    Some(MachineBootInterfaceTarget::Pair(MachineBootInterface {
                        mac_address: mac,
                        interface_id: "NIC.Slot.7-1-1".to_string(),
                    })),
            }

            "legacy MAC only" {
                (Some(mac), None) => Some(MachineBootInterfaceTarget::MacOnly(mac)),
            }

            "empty interface id is MAC only" {
                (Some(mac), Some(String::new())) =>
                    Some(MachineBootInterfaceTarget::MacOnly(mac)),
            }

            "interface id without MAC" {
                (None, Some("NIC.Slot.7-1-1".to_string())) => None,
            }

            "no target parts" {
                (None, None) => None,
            }
        );
    }

    #[test]
    fn target_accessors_return_the_available_identifiers() {
        let mac = MacAddress::new([1, 2, 3, 4, 5, 6]);

        value_scenarios!(run = |target: MachineBootInterfaceTarget| {
            (
                target.mac_address(),
                target.interface_id().map(ToOwned::to_owned),
            )
        };
            "complete pair" {
                MachineBootInterfaceTarget::Pair(MachineBootInterface {
                    mac_address: mac,
                    interface_id: "NIC.Slot.7-1-1".to_string(),
                }) => (
                    mac,
                    Some("NIC.Slot.7-1-1".to_string()),
                ),
            }

            "MAC only" {
                MachineBootInterfaceTarget::MacOnly(mac) => (
                    mac,
                    None,
                ),
            }
        );
    }
}
