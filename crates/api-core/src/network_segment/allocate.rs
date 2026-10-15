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
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use carbide_network::ip::IdentifyAddressFamily;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use ipnetwork::IpNetwork;
use itertools::Itertools;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{AllocationStrategy, NewNetworkSegment};
use model::vpc::vpc_prefix_can_allocate_interface_prefix;
use sqlx::PgConnection;

use crate::{CarbideError, CarbideResult};

/// ip_to_u128 converts an IP address to its u128 representation.
/// IPv4 addresses are zero-extended (u32 → u128), and IPv6 are native u128.
fn ip_to_u128(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(v4) => u32::from(v4) as u128,
        IpAddr::V6(v6) => u128::from(v6),
    }
}

/// u128_to_ip converts a u128 back to an IP address of the appropriate family.
fn u128_to_ip(val: u128, is_v6: bool) -> IpAddr {
    if is_v6 {
        IpAddr::V6(Ipv6Addr::from(val))
    } else {
        IpAddr::V4(Ipv4Addr::from(val as u32))
    }
}

/// PrefixAllocator allocates a prefix of given length from a VPC prefix.
/// Works with both IPv4 and IPv6 address families.
/// Currently used only for FNN linknet allocation.
///
/// The prefix length is validated at construction time via [`PrefixAllocator::new`],
/// so all internal address arithmetic can rely on the prefix
/// being valid for the address family without additional checks.
#[derive(Debug)]
pub(crate) struct PrefixAllocator {
    vpc_prefix_id: VpcPrefixId,
    vpc_prefix: IpNetwork,
    last_used_prefix: Option<IpNetwork>,
    prefix: u8,
}

/// networks_overlap returns true if two networks overlap (as in
/// one contains the other's network address).
fn networks_overlap(a: IpNetwork, b: IpNetwork) -> bool {
    a.contains(b.network()) || b.contains(a.network())
}

/// Returns the number of leading endpoints reserved on a generated linknet.
///
/// IPv4 reserves its DPU endpoint through the explicit gateway. Stateful IPv6
/// cannot persist a gateway, so it reserves `::0` explicitly and allocates
/// `::1` to the instance. SLAAC retains the whole generated prefix without
/// allocating a concrete host address.
fn generated_linknet_num_reserved(prefix: IpNetwork) -> i32 {
    if prefix.is_ipv6() { 1 } else { 0 }
}

/// Finds the first unoccupied linknet index in an inclusive search range.
///
/// `occupied` must be sorted by its inclusive start index. Overlapping ranges
/// need not be merged because advancing past each range is monotonic.
fn first_unoccupied_index(
    occupied: &[(u128, u128)],
    range_start: u128,
    range_end: u128,
) -> Option<u128> {
    if range_start > range_end {
        return None;
    }

    let mut candidate = range_start;
    for &(occupied_start, occupied_end) in occupied {
        if occupied_end < candidate {
            continue;
        }
        if occupied_start > candidate {
            return Some(candidate);
        }

        candidate = occupied_end.saturating_add(1);
        if candidate > range_end {
            return None;
        }
    }
    Some(candidate)
}

impl PrefixAllocator {
    pub(crate) fn new(
        vpc_prefix_id: VpcPrefixId,
        vpc_prefix: IpNetwork,
        last_used_prefix: Option<IpNetwork>,
        prefix: u8,
    ) -> CarbideResult<PrefixAllocator> {
        let max_bits = vpc_prefix.address_family().interface_prefix_len();
        if prefix > max_bits {
            return Err(CarbideError::InvalidArgument(format!(
                "prefix length {prefix} exceeds maximum for address family ({max_bits})"
            )));
        }
        if !vpc_prefix_can_allocate_interface_prefix(
            vpc_prefix.address_family(),
            vpc_prefix.prefix(),
            prefix,
        ) {
            return Err(CarbideError::InvalidArgument(format!(
                "prefix length {prefix} must be greater than VPC prefix length {}, except for an IPv4 /31",
                vpc_prefix.prefix()
            )));
        }

        Ok(Self {
            vpc_prefix_id,
            vpc_prefix,
            last_used_prefix,
            prefix,
        })
    }

    /// Creates a generated segment for a prefix already selected and validated
    /// by this allocator's caller.
    pub(crate) async fn allocate_network_segment_for_prefix(
        &self,
        txn: &mut PgConnection,
        vpc_id: VpcId,
        prefix: IpNetwork,
    ) -> CarbideResult<(NetworkSegmentId, IpNetwork)> {
        let name = format!("vpc_prefix_{}", prefix.network());
        let segment_id = NetworkSegmentId::new();

        let ns = NewNetworkSegment {
            id: segment_id,
            name,
            subdomain_id: None,
            vpc_id: Some(vpc_id),
            mtu: 9000, // Default value.
            prefixes: vec![],
            vlan_id: None,
            vni: None,
            segment_type: model::network_segment::NetworkSegmentType::Tenant,
            can_stretch: Some(false), // All segments allocated here are FNN linknets.
            allocation_strategy: AllocationStrategy::Dynamic,
            infer_slaac_eui64_addresses: false,
        };

        // Segments created by VPC prefix allocation are fully formed (prefixes, VPC ID,
        // gateway, etc.) and don't need the state controller to provision them. Starting
        // in Ready avoids the race where the instance allocator tries to use the segment
        // before the state controller transitions it from Provisioning.
        let segment = db::network_segment::persist(
            ns,
            txn,
            model::network_segment::NetworkSegmentControllerState::Ready,
        )
        .await?;

        self.allocate_linknet_for_segment_with_prefix(txn, segment.id, prefix)
            .await?;

        Ok((segment.id, prefix))
    }

    /// Attaches a prefix already selected and validated by this allocator's
    /// caller to an existing generated segment.
    pub(crate) async fn allocate_linknet_for_segment_with_prefix(
        &self,
        txn: &mut PgConnection,
        segment_id: NetworkSegmentId,
        prefix: IpNetwork,
    ) -> CarbideResult<IpNetwork> {
        // The `no_gateway_on_ipv6` database constraint requires IPv6 prefixes
        // to omit an explicit gateway.
        let gateway = if prefix.is_ipv4() {
            Some(prefix.network())
        } else {
            None
        };

        db::network_prefix::create_for(
            txn,
            &segment_id,
            &[NewNetworkPrefix {
                prefix,
                gateway,
                dhcpv6_link_address: None,
                num_reserved: generated_linknet_num_reserved(prefix),
            }],
            Some(self.vpc_prefix_id),
        )
        .await?;

        Ok(prefix)
    }

    /// Returns the next unoccupied child prefix using the persisted next-fit cursor.
    ///
    /// Occupied ranges are searched as child prefix index intervals so broad
    /// IPv6 allocations do not require enumerating every constituent prefix.
    pub(crate) async fn next_free_prefix(
        &self,
        txn: &mut PgConnection,
    ) -> CarbideResult<IpNetwork> {
        let used_prefixes =
            db::network_prefix::find_allocation_occupancy(txn, self.vpc_prefix_id, self.vpc_prefix)
                .await?
                .into_iter()
                .map(|network_prefix| network_prefix.prefix)
                .collect_vec();

        // `new()` permits equality only for a single IPv4 /31 linknet, so the
        // subtraction remains valid for every constructed allocator.
        let prefix_delta = u32::from(self.prefix - self.vpc_prefix.prefix());
        let total_network_possible = 1u128.checked_shl(prefix_delta).ok_or_else(|| {
            CarbideError::internal(format!(
                "unable to represent the number of /{} linknets in VPC prefix {}",
                self.prefix, self.vpc_prefix
            ))
        })?;
        let max_bits = u32::from(self.vpc_prefix.address_family().interface_prefix_len());
        let host_bits = max_bits - u32::from(self.prefix);
        let linknet_size = 1u128.checked_shl(host_bits).ok_or_else(|| {
            CarbideError::internal(format!(
                "unable to represent a /{} linknet address range",
                self.prefix
            ))
        })?;
        let vpc_start = ip_to_u128(self.vpc_prefix.network());
        let vpc_end = ip_to_u128(self.vpc_prefix.broadcast());
        let is_ipv6 = self.vpc_prefix.is_ipv6();

        // Convert every overlapping network prefix into the inclusive range of
        // linknet indexes that it occupies. Capacity reporting uses this same
        // interval representation, so broad and direct/unparented ranges have
        // identical allocation and utilization semantics.
        let occupied = db::network_prefix::occupied_prefix_intervals(
            self.vpc_prefix,
            self.prefix,
            used_prefixes,
        );

        // Preserve next-fit cursor semantics. An absent or out-of-range cursor
        // starts at the parent prefix; the last linknet wraps to index zero.
        let start_index = self
            .last_used_prefix
            .filter(|prefix| prefix.is_ipv6() == is_ipv6)
            .and_then(|prefix| {
                let prefix_end = ip_to_u128(prefix.broadcast());
                (vpc_start..=vpc_end)
                    .contains(&prefix_end)
                    .then(|| ((prefix_end - vpc_start) / linknet_size + 1) % total_network_possible)
            })
            .unwrap_or_default();

        let free_index = first_unoccupied_index(&occupied, start_index, total_network_possible - 1)
            .or_else(|| {
                start_index
                    .checked_sub(1)
                    .and_then(|range_end| first_unoccupied_index(&occupied, 0, range_end))
            })
            .ok_or_else(|| {
                CarbideError::ResourceExhausted(format!(
                    "VPC prefix {} ({}) has no available /{} linknets",
                    self.vpc_prefix_id, self.vpc_prefix, self.prefix
                ))
            })?;

        let address = free_index
            .checked_mul(linknet_size)
            .and_then(|offset| vpc_start.checked_add(offset))
            .ok_or_else(|| {
                CarbideError::internal(format!(
                    "allocated linknet index {free_index} overflowed VPC prefix {}",
                    self.vpc_prefix
                ))
            })?;
        IpNetwork::new(u128_to_ip(address, is_ipv6), self.prefix).map_err(|error| {
            CarbideError::internal(format!(
                "unable to construct allocated linknet in VPC prefix {}: {error}",
                self.vpc_prefix
            ))
        })
    }

    pub(crate) async fn validate_desired_prefix(
        &self,
        txn: &mut PgConnection,
        prefix: IpNetwork,
    ) -> CarbideResult<()> {
        // Reject if what's being asked for is bigger than the prefix
        // expected to contain it or simply not contained within it at all.
        // (i.e. an IP from a totally different prefix)
        if !self.vpc_prefix.contains(prefix.network()) {
            return Err(CarbideError::InvalidArgument(format!(
                "{prefix} is not contained within {}",
                self.vpc_prefix
            )));
        }

        // Reject if already in use.
        if db::network_prefix::find_allocation_occupancy(txn, self.vpc_prefix_id, self.vpc_prefix)
            .await?
            .iter()
            .any(|network_prefix| networks_overlap(network_prefix.prefix, prefix))
        {
            return Err(CarbideError::AlreadyFoundError {
                kind: "prefix",
                id: prefix.to_string(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use carbide_test_support::value_scenarios;
    use carbide_uuid::vpc::VpcPrefixId;

    use crate::network_segment::allocate::{PrefixAllocator, first_unoccupied_index};

    #[test]
    fn validates_linknet_prefix_length() {
        value_scenarios!(
            run = |(parent, prefix): (&str, u8)| {
                PrefixAllocator::new(
                    VpcPrefixId::new(),
                    parent.parse().unwrap(),
                    None,
                    prefix,
                )
                .is_ok()
            };
            "valid child prefixes" {
                ("192.0.2.0/30", 31) => true,
                ("192.0.2.0/31", 31) => true,
                ("2001:db8::/63", 64) => true,
                ("2001:db8::/126", 127) => true,
            }

            "invalid child prefixes" {
                ("192.0.2.0/30", 30) => false,
                ("192.0.2.0/31", 30) => false,
                ("192.0.2.1/32", 32) => false,
                ("192.0.2.0/24", 33) => false,
                ("2001:db8::/64", 64) => false,
                ("2001:db8::/126", 64) => false,
                ("2001:db8::/127", 127) => false,
                ("2001:db8::1/128", 128) => false,
                ("2001:db8::/64", 129) => false,
            }
        );
    }

    /// Exercises interval lookup without enumerating every possible linknet.
    #[test]
    fn find_first_unoccupied_linknet_index() {
        // These cases cover gaps, overlapping occupied ranges, and complete
        // exhaustion without constructing an address space proportional to IPv6.
        let cases = [
            ("empty", vec![], 0, 7, Some(0)),
            ("leading occupied", vec![(0, 3)], 0, 7, Some(4)),
            ("overlapping occupied", vec![(0, 3), (2, 6)], 0, 7, Some(7)),
            ("gap", vec![(0, 1), (4, 7)], 0, 7, Some(2)),
            ("exhausted", vec![(0, 7)], 0, 7, None),
        ];

        for (name, occupied, range_start, range_end, expected) in cases {
            assert_eq!(
                first_unoccupied_index(&occupied, range_start, range_end),
                expected,
                "case: {name}",
            );
        }
    }

    #[test]
    fn historical_stateful_prefix_occupies_its_slaac_allocation() {
        let parent = "2001:db8::/63".parse().unwrap();
        let historical_prefix = "2001:db8::/127".parse().unwrap();
        let occupied =
            db::network_prefix::occupied_prefix_intervals(parent, 64, [historical_prefix]);

        assert_eq!(occupied, vec![(0, 0)]);
        assert_eq!(first_unoccupied_index(&occupied, 0, 1), Some(1));
    }
}
