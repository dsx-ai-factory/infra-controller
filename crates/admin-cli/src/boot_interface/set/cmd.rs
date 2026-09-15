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

//! Set a managed host's boot interface by promoting the chosen interface to
//! the host's primary -- the designation `pick_boot_interface` keys on. A
//! thin front for the same `SetPrimaryInterface` RPC behind
//! `managed-host set-primary-interface`: the server commits the selected row
//! and desired target together, then machine-controller converges Redfish.
//! The only client-side work is resolving an operator-entered MAC to its
//! managed interface row.

use ::rpc::forge as forgerpc;
use carbide_uuid::machine::MachineInterfaceId;
use mac_address::MacAddress;

use super::args::{Args, InterfaceSelector};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn handle_set(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let interface_id = match args.interface {
        InterfaceSelector::Id(id) => id,
        InterfaceSelector::Mac(mac) => {
            let response = api_client
                .get_machine_boot_interfaces(args.machine.into())
                .await?;
            resolve_mac_to_interface_id(&response, mac)?
        }
    };

    #[allow(deprecated)] // Keep `--reboot` functional when this CLI calls an older server.
    let request = forgerpc::SetPrimaryInterfaceRequest {
        host_machine_id: Some(args.machine),
        interface_id: Some(interface_id),
        reboot: args.reboot,
        force_reconcile: args.force_reconcile || args.reboot,
    };
    api_client.0.set_primary_interface(request).await?;
    Ok(())
}

/// Resolve an operator-entered MAC to the machine's one managed interface row
/// with that MAC. MACs are unique per segment, not per machine, so a MAC that
/// matches several rows is refused rather than guessed at -- the UUID names a
/// row exactly.
fn resolve_mac_to_interface_id(
    response: &forgerpc::GetMachineBootInterfacesResponse,
    mac: MacAddress,
) -> CarbideCliResult<MachineInterfaceId> {
    let matches: Vec<&forgerpc::MachineInterfaceBootInterface> = response
        .machine_interfaces
        .iter()
        .filter(|i| i.mac_address.parse::<MacAddress>().ok() == Some(mac))
        .collect();

    match matches.as_slice() {
        [] => Err(CarbideCliError::GenericError(format!(
            "no managed interface with MAC {mac} on this machine -- \
             `boot-interface candidates` lists its interfaces. A machine still \
             waiting on its first DHCP lease has predictions only; declare its \
             boot NIC via the expected machine's `interfaces[].primary` instead"
        ))),
        [only] => only.interface_id.ok_or_else(|| {
            CarbideCliError::GenericError(
                "the API server did not report interface row ids for this machine; \
                 pass the machine-interface UUID instead of a MAC"
                    .to_string(),
            )
        }),
        several => Err(CarbideCliError::GenericError(format!(
            "MAC {mac} matches {} interfaces on this machine (the same MAC can \
             exist on several segments); pass the machine-interface UUID instead: {}",
            several.len(),
            several
                .iter()
                .map(|i| {
                    i.interface_id
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "<unknown id>".to_string())
                })
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn managed_row(
        mac: &str,
        interface_id: Option<&str>,
    ) -> forgerpc::MachineInterfaceBootInterface {
        forgerpc::MachineInterfaceBootInterface {
            mac_address: mac.to_string(),
            primary_interface: false,
            boot_interface_id: None,
            network_segment_type: Some("host_inband".to_string()),
            interface_id: interface_id.map(|id| id.parse().expect("test UUIDs parse")),
        }
    }

    fn response_with(
        rows: Vec<forgerpc::MachineInterfaceBootInterface>,
    ) -> forgerpc::GetMachineBootInterfacesResponse {
        forgerpc::GetMachineBootInterfacesResponse {
            machine_id: None,
            machine_interfaces: rows,
            predicted_interfaces: vec![],
            explored_endpoints: vec![],
            retained_interfaces: vec![],
            effective_boot_interface_mac: None,
            effective_boot_interface_id: None,
            divergent: false,
            default_boot_interface: None,
            predicted_boot_interface: None,
            reconciliation: None,
        }
    }

    #[test]
    fn a_unique_mac_resolves_to_its_row_id() {
        let response = response_with(vec![
            managed_row(
                "00:11:22:33:44:55",
                Some("abcdef01-2345-6789-abcd-ef0123456789"),
            ),
            managed_row(
                "00:11:22:33:44:66",
                Some("12345678-1234-5678-90ab-cdef01234567"),
            ),
        ]);

        let resolved = resolve_mac_to_interface_id(&response, "00:11:22:33:44:55".parse().unwrap())
            .expect("a unique MAC resolves");
        assert_eq!(resolved.to_string(), "abcdef01-2345-6789-abcd-ef0123456789");
    }

    #[test]
    fn an_unknown_mac_is_refused_with_guidance() {
        let response = response_with(vec![managed_row(
            "00:11:22:33:44:66",
            Some("12345678-1234-5678-90ab-cdef01234567"),
        )]);

        let err = resolve_mac_to_interface_id(&response, "00:11:22:33:44:55".parse().unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("no managed interface with MAC"));
    }

    #[test]
    fn a_duplicate_mac_is_refused_not_guessed() {
        // The same MAC on two segments -- both rows are real, so the resolver
        // must refuse and point at the UUIDs rather than pick one.
        let response = response_with(vec![
            managed_row(
                "00:11:22:33:44:55",
                Some("abcdef01-2345-6789-abcd-ef0123456789"),
            ),
            managed_row(
                "00:11:22:33:44:55",
                Some("12345678-1234-5678-90ab-cdef01234567"),
            ),
        ]);

        let err = resolve_mac_to_interface_id(&response, "00:11:22:33:44:55".parse().unwrap())
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("matches 2 interfaces"));
        assert!(message.contains("abcdef01-2345-6789-abcd-ef0123456789"));
        assert!(message.contains("12345678-1234-5678-90ab-cdef01234567"));
    }

    #[test]
    fn a_row_without_a_reported_id_asks_for_the_uuid() {
        // An older API server that predates row-id reporting: the MAC matches,
        // but there is no UUID to hand to SetPrimaryInterface.
        let response = response_with(vec![managed_row("00:11:22:33:44:55", None)]);

        let err = resolve_mac_to_interface_id(&response, "00:11:22:33:44:55".parse().unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("pass the machine-interface UUID"));
    }
}
