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

use std::str::FromStr;

use carbide_uuid::machine::{MachineInterfaceId, StableHostMachineId};
use clap::Parser;
use mac_address::MacAddress;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Set the boot interface by MAC address (must match exactly one managed interface):
    $ nico-admin-cli boot-interface set 12345678-1234-5678-90ab-cdef01234567 00:11:22:33:44:55

Set it by machine-interface UUID (exact, works even with duplicate MACs):
    $ nico-admin-cli boot-interface set 12345678-1234-5678-90ab-cdef01234567 \
    abcdef01-2345-6789-abcd-ef0123456789

Request another reconciliation for the selected interface:
    $ nico-admin-cli boot-interface set 12345678-1234-5678-90ab-cdef01234567 \
    00:11:22:33:44:55 --force-reconcile

Tip: 'boot-interface candidates <MACHINE_ID>' lists the candidate NICs with their MACs and UUIDs.
")]
pub(crate) struct Args {
    #[clap(help = "The managed host for which to set the boot interface")]
    pub(super) machine: StableHostMachineId,
    #[clap(help = "The interface to boot from -- a machine-interface UUID or a MAC address")]
    pub(super) interface: InterfaceSelector,
    #[clap(
        long,
        help = "Request a fresh machine-controller reconciliation even when this interface is already selected. Sends only force_reconcile=true; servers without force_reconcile support ignore it, while supporting servers leave any required restart to machine-controller"
    )]
    pub(super) force_reconcile: bool,
    #[clap(
        long,
        help = "Deprecated compatibility option for servers without force_reconcile support. Sends reboot=true and force_reconcile=true; supporting servers treat it as reconciliation, while older servers force-restart the host after changing the target"
    )]
    pub(super) reboot: bool,
}

/// How the operator names the target interface: the `machine_interfaces` row
/// UUID (exact), or the NIC's MAC address (resolved against the machine's
/// managed rows, refused when ambiguous).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum InterfaceSelector {
    Id(MachineInterfaceId),
    Mac(MacAddress),
}

impl FromStr for InterfaceSelector {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(id) = s.parse::<MachineInterfaceId>() {
            return Ok(Self::Id(id));
        }
        if let Ok(mac) = s.parse::<MacAddress>() {
            return Ok(Self::Mac(mac));
        }
        Err(format!(
            "`{s}` is neither a machine-interface UUID nor a MAC address"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_parses_a_uuid_as_an_interface_id() {
        let parsed: InterfaceSelector = "abcdef01-2345-6789-abcd-ef0123456789".parse().unwrap();
        assert!(matches!(parsed, InterfaceSelector::Id(_)));
    }

    #[test]
    fn selector_parses_a_mac_address() {
        let parsed: InterfaceSelector = "00:11:22:33:44:55".parse().unwrap();
        assert_eq!(
            parsed,
            InterfaceSelector::Mac("00:11:22:33:44:55".parse().unwrap())
        );
    }

    #[test]
    fn selector_rejects_anything_else() {
        let err = "not-an-interface".parse::<InterfaceSelector>().unwrap_err();
        assert!(err.contains("neither a machine-interface UUID nor a MAC address"));
    }
}
