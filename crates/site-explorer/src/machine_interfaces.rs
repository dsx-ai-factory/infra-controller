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

use db::machine_interface::MachineInterfaces;
use model::machine_boot_interface::MachineBootInterfaceTarget;

/// Site Explorer's boot-target proposal from settled interface ownership.
pub(super) trait MachineInterfacesExt {
    /// Proposes an initial target or a Redfish ID for the already designated MAC.
    /// Returns `None` if the target is complete or no suitable proposal can be made.
    fn proposed_boot_interface_target(
        &self,
        desired: Option<&MachineBootInterfaceTarget>,
    ) -> Option<MachineBootInterfaceTarget>;
}

impl MachineInterfacesExt for MachineInterfaces {
    fn proposed_boot_interface_target(
        &self,
        desired: Option<&MachineBootInterfaceTarget>,
    ) -> Option<MachineBootInterfaceTarget> {
        match desired {
            Some(MachineBootInterfaceTarget::Pair(_)) => None,
            Some(MachineBootInterfaceTarget::MacOnly(mac_address)) => self
                .boot_interface_for_mac(*mac_address)
                .map(MachineBootInterfaceTarget::Pair),
            None => {
                // An explicit primary prediction outranks an interim owned interface.
                let prediction = self.boot_prediction();
                let selected_mac = prediction
                    .filter(|prediction| prediction.primary_interface)
                    .map(|prediction| prediction.mac_address)
                    .or_else(|| self.boot_interface().map(|interface| interface.mac_address))
                    .or_else(|| prediction.map(|prediction| prediction.mac_address));
                selected_mac.map(|mac_address| {
                    self.boot_interface_for_mac(mac_address).map_or(
                        MachineBootInterfaceTarget::MacOnly(mac_address),
                        MachineBootInterfaceTarget::Pair,
                    )
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::{Check, check_values};
    use carbide_uuid::machine::MachineId;
    use mac_address::MacAddress;
    use model::machine::MachineInterfaceSnapshot;
    use model::machine_boot_interface::MachineBootInterface;
    use model::machine_interface::InterfaceType;
    use model::network_segment::NetworkSegmentType;
    use model::predicted_machine_interface::PredictedMachineInterface;

    use super::*;

    /// Proposals initialize a missing target or complete the same designated MAC.
    /// These cases keep the
    /// prediction/owned precedence and the ambiguity rules together.
    #[test]
    fn proposed_boot_interface_target_cases() {
        let mac = |last| MacAddress::new([0x02, 0, 0, 0, 0, last]);
        let interface =
            |mac_address, interface_type, primary_interface, interface_id: Option<&str>| {
                MachineInterfaceSnapshot {
                    interface_type,
                    primary_interface,
                    boot_interface_id: interface_id.map(str::to_string),
                    network_segment_type: Some(NetworkSegmentType::HostInband),
                    ..MachineInterfaceSnapshot::mock_with_mac(mac_address)
                }
            };
        let prediction = |mac_address, primary_interface, interface_id: Option<&str>| {
            PredictedMachineInterface {
                id: uuid::Uuid::nil(),
                machine_id: MachineId::from_str(
                    "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng",
                )
                .unwrap(),
                mac_address,
                expected_network_segment_type: NetworkSegmentType::HostInband,
                boot_interface_id: interface_id.map(str::to_string),
                primary_interface,
            }
        };
        let pair = |mac_address, interface_id: &str| {
            MachineBootInterfaceTarget::Pair(MachineBootInterface {
                mac_address,
                interface_id: interface_id.to_string(),
            })
        };

        let owned_mac = mac(1);
        let predicted_mac = mac(2);
        let desired_mac = mac(3);

        check_values(
            [
                Check {
                    scenario: "explicit primary prediction outranks an owned primary",
                    input: (
                        None,
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(predicted_mac, "NIC.Predicted")),
                },
                Check {
                    scenario: "selected prediction uses a same-MAC owned id",
                    input: (
                        None,
                        vec![interface(
                            predicted_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, None)],
                    ),
                    expect: Some(pair(predicted_mac, "NIC.Owned")),
                },
                Check {
                    scenario: "owned candidate outranks a non-primary prediction",
                    input: (
                        None,
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(owned_mac, "NIC.Owned")),
                },
                Check {
                    scenario: "a sole prediction initializes a MAC-only target",
                    input: (None, vec![], vec![prediction(predicted_mac, false, None)]),
                    expect: Some(MachineBootInterfaceTarget::MacOnly(predicted_mac)),
                },
                Check {
                    scenario: "a whitespace-only id initializes a MAC-only target",
                    input: (
                        None,
                        vec![],
                        vec![prediction(predicted_mac, false, Some("\t\n"))],
                    ),
                    expect: Some(MachineBootInterfaceTarget::MacOnly(predicted_mac)),
                },
                Check {
                    scenario: "several non-primary predictions do not initialize a target",
                    input: (
                        None,
                        vec![],
                        vec![
                            prediction(predicted_mac, false, Some("NIC.Predicted")),
                            prediction(desired_mac, false, Some("NIC.Other")),
                        ],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "an existing pair is never rewritten",
                    input: (
                        Some(pair(desired_mac, "NIC.Remembered")),
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, Some("NIC.Predicted"))],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "a MAC-only target uses a unique owned id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(
                            desired_mac,
                            InterfaceType::Data,
                            false,
                            Some("NIC.Remembered"),
                        )],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Remembered")),
                },
                Check {
                    scenario: "a MAC-only target falls back to a predicted id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(desired_mac, InterfaceType::Data, false, None)],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Predicted")),
                },
                Check {
                    scenario: "a whitespace-only owned id falls back to a predicted id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(
                            desired_mac,
                            InterfaceType::Data,
                            false,
                            Some("\t\n"),
                        )],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Predicted")),
                },
                Check {
                    scenario: "ambiguous owned ids stop enrichment",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![
                            interface(desired_mac, InterfaceType::Data, false, Some("NIC.Owned.1")),
                            interface(desired_mac, InterfaceType::Data, false, Some("NIC.Owned.2")),
                        ],
                        vec![prediction(desired_mac, false, Some("NIC.Predicted"))],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "duplicate owned ids are still one unique id",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![
                            interface(
                                desired_mac,
                                InterfaceType::Data,
                                false,
                                Some(" \tNIC.Owned\n "),
                            ),
                            interface(desired_mac, InterfaceType::Data, false, Some("NIC.Owned")),
                        ],
                        vec![],
                    ),
                    expect: Some(pair(desired_mac, "NIC.Owned")),
                },
                Check {
                    scenario: "ambiguous predicted ids stop enrichment",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![],
                        vec![
                            prediction(desired_mac, false, Some("NIC.Predicted.1")),
                            prediction(desired_mac, false, Some("NIC.Predicted.2")),
                        ],
                    ),
                    expect: None,
                },
                Check {
                    scenario: "a different designated MAC is preserved",
                    input: (
                        Some(MachineBootInterfaceTarget::MacOnly(desired_mac)),
                        vec![interface(
                            owned_mac,
                            InterfaceType::Data,
                            true,
                            Some("NIC.Owned"),
                        )],
                        vec![prediction(predicted_mac, true, Some("NIC.Predicted"))],
                    ),
                    expect: None,
                },
            ],
            |(desired, interfaces, predictions)| {
                MachineInterfaces {
                    interfaces,
                    predictions,
                }
                .proposed_boot_interface_target(desired.as_ref())
            },
        );
    }
}
