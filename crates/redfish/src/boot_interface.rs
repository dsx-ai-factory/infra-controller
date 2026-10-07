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
//! Helpers for running boot-interface-targeted Redfish operations.

use std::future::Future;

use ::libredfish::{BootInterfaceRef, RedfishError};
use mac_address::MacAddress;
use model::machine_boot_interface::{MachineBootInterface, MachineBootInterfaceTarget};

/// How to target a host's boot interface for a Redfish setup operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BootInterfaceTarget {
    /// A fully captured boot interface (MAC + Redfish `interface_id`). Core
    /// supplies both identifiers to `libredfish` as one
    /// [`BootInterfaceRef::Pair`], allowing each vendor to use its native
    /// identifier.
    Pair(MachineBootInterface),
    /// Only a MAC is known -- the boot interface has never been captured with an
    /// `interface_id` (e.g. a host last explored before id capture, or never
    /// reported with a resolvable `interface_id`).
    MacOnly(MacAddress),
}

impl BootInterfaceTarget {
    /// Runs `op` against this boot interface. For [`BootInterfaceTarget::Pair`]
    /// both identifiers are supplied in one call; for
    /// [`BootInterfaceTarget::MacOnly`] only the MAC is supplied.
    ///
    /// `op` is invoked with a [`BootInterfaceRef`] and should call the desired
    /// Redfish trait method with it, e.g.
    /// `|bi| client.set_boot_order_dpu_first(bi)` or
    /// `|bi| client.is_bios_setup(Some(bi))`.
    pub async fn run<'s, T, F, Fut>(&'s self, op: F) -> Result<T, RedfishError>
    where
        F: FnOnce(BootInterfaceRef<'s>) -> Fut,
        Fut: Future<Output = Result<T, RedfishError>>,
    {
        match self {
            BootInterfaceTarget::Pair(boot_interface) => {
                op(BootInterfaceRef::Pair {
                    mac_address: boot_interface.mac_address,
                    interface_id: &boot_interface.interface_id,
                })
                .await
            }
            BootInterfaceTarget::MacOnly(mac) => op(BootInterfaceRef::Mac(*mac)).await,
        }
    }

    /// The MAC of this boot interface (always present for both variants).
    pub fn mac_address(&self) -> MacAddress {
        match self {
            BootInterfaceTarget::Pair(boot_interface) => boot_interface.mac_address,
            BootInterfaceTarget::MacOnly(mac) => *mac,
        }
    }
}

impl From<MachineBootInterfaceTarget> for BootInterfaceTarget {
    fn from(target: MachineBootInterfaceTarget) -> Self {
        match target {
            MachineBootInterfaceTarget::Pair(boot_interface) => Self::Pair(boot_interface),
            MachineBootInterfaceTarget::MacOnly(mac_address) => Self::MacOnly(mac_address),
        }
    }
}

impl From<BootInterfaceTarget> for MachineBootInterfaceTarget {
    fn from(target: BootInterfaceTarget) -> Self {
        match target {
            BootInterfaceTarget::Pair(boot_interface) => Self::Pair(boot_interface),
            BootInterfaceTarget::MacOnly(mac_address) => Self::MacOnly(mac_address),
        }
    }
}

impl From<&BootInterfaceTarget> for MachineBootInterfaceTarget {
    fn from(target: &BootInterfaceTarget) -> Self {
        target.clone().into()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_test_support::value_scenarios;

    use super::*;

    fn mac() -> MacAddress {
        MacAddress::new([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0x01])
    }

    #[tokio::test]
    async fn pair_target_runs_once_when_operation_fails() {
        let attempts = AtomicUsize::new(0);
        let target = BootInterfaceTarget::Pair(MachineBootInterface {
            mac_address: mac(),
            interface_id: "NIC.Slot.7-1-1".to_string(),
        });
        let result: Result<(), RedfishError> = target
            .run(|boot_interface| {
                attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    match boot_interface {
                        BootInterfaceRef::Pair {
                            mac_address,
                            interface_id,
                        } => {
                            assert_eq!(mac_address, mac());
                            assert_eq!(interface_id, "NIC.Slot.7-1-1");
                        }
                        _ => panic!("paired target must supply both identifiers"),
                    }
                    Err(RedfishError::NoContent)
                }
            })
            .await;

        assert!(matches!(result, Err(RedfishError::NoContent)));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn mac_only_target_never_uses_interface_id() {
        let target = BootInterfaceTarget::MacOnly(mac());
        let result: Result<(), RedfishError> = target
            .run(|bi| async move {
                assert!(matches!(bi, BootInterfaceRef::Mac(_)));
                Ok(())
            })
            .await;
        assert!(result.is_ok());
    }

    #[test]
    fn model_and_redfish_targets_preserve_both_selector_forms() {
        value_scenarios!(run = |target: MachineBootInterfaceTarget| {
            let redfish = BootInterfaceTarget::from(target);
            let round_trip = MachineBootInterfaceTarget::from(redfish.clone());
            (redfish, round_trip)
        };
            "complete pair" {
                MachineBootInterfaceTarget::Pair(MachineBootInterface {
                    mac_address: mac(),
                    interface_id: "NIC.Slot.7-1-1".to_string(),
                }) => (
                    BootInterfaceTarget::Pair(MachineBootInterface {
                        mac_address: mac(),
                        interface_id: "NIC.Slot.7-1-1".to_string(),
                    }),
                    MachineBootInterfaceTarget::Pair(MachineBootInterface {
                        mac_address: mac(),
                        interface_id: "NIC.Slot.7-1-1".to_string(),
                    }),
                ),
            }

            "legacy MAC only" {
                MachineBootInterfaceTarget::MacOnly(mac()) => (
                    BootInterfaceTarget::MacOnly(mac()),
                    MachineBootInterfaceTarget::MacOnly(mac()),
                ),
            }
        );
    }
}
