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
//! DHCPv6 SLAAC helpers used by `crate::dhcp::discover`.

use std::net::{IpAddr, Ipv6Addr};

use carbide_network::ip::IpAddressFamily;
use carbide_uuid::machine::MachineInterfaceId;
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::network_segment::NetworkSegment;
use sqlx::PgConnection;

use crate::CarbideError;

/// Compute the SLAAC address formed by applying RFC 4291 EUI-64 to `mac`.
///
/// Returns `None` unless `prefix` is an IPv6 /64, because modified EUI-64
/// interface identifiers are defined for 64-bit subnet prefixes.
fn slaac_gua_from_eui64(prefix: &IpNetwork, mac: &MacAddress) -> Option<Ipv6Addr> {
    // SLAAC EUI-64 is only meaningful for IPv6 /64 prefixes.
    let IpNetwork::V6(prefix) = prefix else {
        return None;
    };
    if prefix.prefix() != 64 {
        return None;
    }

    // Copy the network bits, then append the modified EUI-64 identifier.
    let mac = mac.bytes();
    let mut octets = prefix.network().octets();
    octets[8] = mac[0] ^ 0x02;
    octets[9] = mac[1];
    octets[10] = mac[2];
    octets[11] = 0xff;
    octets[12] = 0xfe;
    octets[13] = mac[3];
    octets[14] = mac[4];
    octets[15] = mac[5];

    Some(Ipv6Addr::from(octets))
}

/// Infer and persist one modified EUI-64 SLAAC address when the segment
/// permits it.
///
/// A row is inserted only when the segment opts in and has exactly one IPv6
/// prefix whose length is /64. Stateful DHCPv6 or static IPv6 assignments
/// suppress inference. Opted-out and reserved segments still serve DHCPv6
/// options without creating an inferred address row.
pub(super) async fn infer_slaac_eui64_address(
    txn: &mut PgConnection,
    interface_id: MachineInterfaceId,
    segment: &NetworkSegment,
    mac: &MacAddress,
) -> Result<(), CarbideError> {
    // `NetworkSegment::slaac_eui64_inference_prefix` owns the full segment
    // policy. Keeping the opt-in beside the prefix checks means a stateless
    // DHCPv6 request cannot persist an inferred address through a second path.
    let Some(prefix) = segment.slaac_eui64_inference_prefix() else {
        tracing::debug!(
            network_segment_id = %segment.id,
            allocation_strategy = ?segment.config.allocation_strategy,
            infer_slaac_eui64_addresses = segment.config.infer_slaac_eui64_addresses,
            prefixes = ?segment.prefixes,
            "DHCPv6 EUI-64 inference skipped because the segment does not permit it"
        );
        return Ok(());
    };

    // Read the family after taking the interface row lock: a DHCPv6 or
    // static assignment may commit while inference waits for this interface.
    let current_segment_id =
        db::machine_interface::lock_for_address_assignment(&mut *txn, interface_id).await?;
    if db::machine_interface_address::has_address_for_family(
        &mut *txn,
        interface_id,
        IpAddressFamily::Ipv6,
    )
    .await?
    {
        return Ok(());
    }

    if current_segment_id != segment.id {
        return Err(CarbideError::FailedPrecondition(format!(
            "network segment changed for interface {interface_id} during SLAAC inference"
        )));
    }

    // Persist the inferred address and refresh DNS naming from the new state.
    if let Some(address) = slaac_gua_from_eui64(prefix, mac) {
        let address = IpAddr::V6(address);

        // The shared insert enforces global ownership of this exact address,
        // including concurrent claims from another interface.
        db::machine_interface_address::insert(
            &mut *txn,
            interface_id,
            address,
            AllocationType::Slaac,
        )
        .await?;
        db::machine_interface::sync_hostname_after_address_assignment(
            txn,
            interface_id,
            segment.config.subdomain_id,
        )
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::value_scenarios;

    use super::*;

    // SLAAC EUI-64: a valid /64 receives the modified EUI-64 identifier, while
    // non-/64 and IPv4 prefixes are intentionally ineligible.
    #[test]
    fn computes_slaac_gua_from_eui64() {
        value_scenarios!(
            run = |(prefix, mac): (&str, &str)| {
                let prefix = IpNetwork::from_str(prefix).unwrap();
                let mac = MacAddress::from_str(mac).unwrap();
                slaac_gua_from_eui64(&prefix, &mac)
            };
            "ipv6 /64" {
                ("2001:db8::/64", "02:00:00:00:00:01") => Some(Ipv6Addr::from_str("2001:db8::ff:fe00:1").unwrap()),
            }

            "ineligible prefixes" {
                ("2001:db8::/80", "02:00:00:00:00:01") => None,
                ("192.0.2.0/24", "02:00:00:00:00:01") => None,
            }
        );
    }
}
