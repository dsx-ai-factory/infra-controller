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

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnetwork::IpNetwork;
use model::network_prefix::NetworkPrefix;
use sqlx::FromRow;

use crate::{DatabaseError, DatabaseResult};

// Each row retains the immediate successor from the complete occupied set.
// Filtering out adjacent rows must not make their addresses appear free.
#[derive(FromRow)]
pub(super) struct OccupiedAddress {
    address: IpAddr,
    next_address: Option<IpAddr>,
}

/// `HostAddressCandidates` counts free addresses and selects them from gaps
/// in the same eligible interval. It does not enumerate the prefix, even
/// for an IPv6 /0.
/// IPv6 reservations mirror `IpAllocator`; IPv4 retains the machine-interface
/// policy, which also reserves an implicit gateway when none is configured.
pub(super) struct HostAddressCandidates<'a> {
    prefix: &'a NetworkPrefix,
    first: Option<u128>,
    last: u128,
}

impl<'a> HostAddressCandidates<'a> {
    pub(super) fn new(prefix: &'a NetworkPrefix) -> DatabaseResult<Self> {
        let (reserved, last) = match prefix.prefix {
            IpNetwork::V4(network) => {
                // IPv4 reserves the second address when no gateway is configured,
                // and always excludes the final address, including /31 and /32.
                let minimum_reserved = if prefix.gateway.is_some() { 1 } else { 2 };
                (
                    prefix.num_reserved.max(minimum_reserved) as u128,
                    u128::from(u32::from(network.broadcast())).saturating_sub(1),
                )
            }
            IpNetwork::V6(network) => {
                let reserved = u128::try_from(prefix.num_reserved).map_err(|_| {
                    DatabaseError::internal(format!(
                        "network prefix {} has negative reservation count {}",
                        prefix.id, prefix.num_reserved
                    ))
                })?;
                // Both endpoints remain usable for IPv6 /127 and /128 prefixes.
                let reserve_endpoints = u128::from(network.prefix() < 127);
                (
                    reserved.max(reserve_endpoints),
                    u128::from(network.broadcast()) - reserve_endpoints,
                )
            }
        };
        Ok(Self {
            prefix,
            first: address_number(prefix.prefix.network()).checked_add(reserved),
            last,
        })
    }

    pub(super) fn range(&self) -> Option<(IpAddr, IpAddr)> {
        self.first
            .filter(|first| *first <= self.last)
            .map(|first| (self.address(first), self.address(self.last)))
    }

    pub(super) fn num_free(&self, used_ips: &[IpAddr]) -> u128 {
        let Some((first, last)) = self.range() else {
            return 0;
        };
        let unavailable = used_ips
            .iter()
            .copied()
            .chain(self.prefix.gateway)
            .chain(self.prefix.svi_ip)
            .filter(|address| (first..=last).contains(address))
            .collect::<HashSet<_>>();

        // IPv6 /0 excludes both endpoints, so the interval fits in u128.
        // Count an owned gateway or SVI once, and only inside that interval.
        address_number(last) - address_number(first) + 1 - unavailable.len() as u128
    }

    // A leading gap this large fills the batch even if it includes both the
    // gateway and SVI. Keep the threshold in range before converting to IPv4.
    pub(super) fn leading_gap_threshold(&self, limit: usize) -> Option<IpAddr> {
        let threshold = self.first?.checked_add(limit as u128)?.checked_add(2)?;
        (threshold <= self.last).then(|| self.address(threshold))
    }

    /// Advance past an address returned by this selector's current `select` call.
    pub(super) fn advance_past(&mut self, address: IpAddr) {
        self.first = address_number(address).checked_add(1);
    }

    /// Rows must be ordered, begin with the first occupied address in `range()`,
    /// and retain each returned address's immediate occupied successor. An empty
    /// result means the range is empty of occupied addresses. For nonempty
    /// results, only a row without a successor proves the trailing range is free.
    pub(super) fn select(&self, occupied: &[OccupiedAddress], limit: usize) -> Vec<IpAddr> {
        let mut candidates = Vec::with_capacity(limit);
        self.extend_range(
            &mut candidates,
            self.first,
            occupied.first().map(|row| address_number(row.address)),
            limit,
        );
        for row in occupied {
            self.extend_range(
                &mut candidates,
                address_number(row.address).checked_add(1),
                row.next_address.map(address_number),
                limit,
            );
        }
        candidates
    }

    // The upper bound is exclusive; None extends through the final usable
    // address. Checked increments also allow the maximum IPv6 host address.
    fn extend_range(
        &self,
        candidates: &mut Vec<IpAddr>,
        mut next: Option<u128>,
        before: Option<u128>,
        limit: usize,
    ) {
        while let Some(number) = next {
            if number > self.last
                || before.is_some_and(|before| number >= before)
                || candidates.len() == limit
            {
                break;
            }
            let address = self.address(number);
            if Some(address) != self.prefix.gateway && Some(address) != self.prefix.svi_ip {
                candidates.push(address);
            }
            next = number.checked_add(1);
        }
    }

    fn address(&self, number: u128) -> IpAddr {
        match self.prefix.prefix {
            IpNetwork::V4(_) => IpAddr::V4(Ipv4Addr::from(number as u32)),
            IpNetwork::V6(_) => IpAddr::V6(Ipv6Addr::from(number)),
        }
    }
}

fn address_number(address: IpAddr) -> u128 {
    match address {
        IpAddr::V4(address) => u128::from(u32::from(address)),
        IpAddr::V6(address) => u128::from(address),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, Check, check_cases, check_values};
    use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};

    use super::*;

    #[derive(Debug)]
    struct Input {
        prefix: &'static str,
        num_reserved: i32,
        gateway: Option<&'static str>,
        svi: Option<&'static str>,
        occupied: &'static [(&'static str, Option<&'static str>)],
        limit: usize,
    }

    #[test]
    fn free_counts_use_host_bounds_and_distinct_exclusions() {
        #[derive(Debug)]
        struct CountInput {
            prefix: &'static str,
            num_reserved: i32,
            gateway: Option<&'static str>,
            svi: Option<&'static str>,
            used_ips: &'static [&'static str],
        }

        check_cases(
            [
                Case {
                    scenario: "unowned gateway and SVI addresses remain unavailable",
                    input: CountInput {
                        prefix: "192.0.2.0/29",
                        num_reserved: 0,
                        gateway: Some("192.0.2.2"),
                        svi: Some("192.0.2.3"),
                        used_ips: &[],
                    },
                    expect: Yields(4),
                },
                Case {
                    scenario: "IPv4 /31 has no host addresses even with a configured gateway",
                    input: CountInput {
                        prefix: "192.0.2.0/31",
                        num_reserved: 0,
                        gateway: Some("192.0.2.0"),
                        svi: None,
                        used_ips: &[],
                    },
                    expect: Yields(0),
                },
                Case {
                    scenario: "IPv4 /32 has no host addresses",
                    input: CountInput {
                        prefix: "0.0.0.0/32",
                        num_reserved: 0,
                        gateway: None,
                        svi: None,
                        used_ips: &[],
                    },
                    expect: Yields(0),
                },
                Case {
                    scenario: "only distinct exclusions within the host interval reduce its count",
                    input: CountInput {
                        prefix: "192.0.2.0/29",
                        num_reserved: 3,
                        gateway: Some("192.0.2.3"),
                        svi: Some("192.0.2.2"),
                        used_ips: &[
                            "192.0.2.0",
                            "192.0.2.1",
                            "192.0.2.3",
                            "192.0.2.3",
                            "192.0.2.4",
                            "192.0.2.7",
                            "192.0.3.4",
                            "::4",
                        ],
                    },
                    expect: Yields(2),
                },
                Case {
                    scenario: "IPv6 /0 counts a large reserved range without enumerating it",
                    input: CountInput {
                        prefix: "::/0",
                        num_reserved: i32::MAX,
                        gateway: None,
                        svi: None,
                        used_ips: &["::7fff:ffff"],
                    },
                    expect: Yields(u128::MAX - i32::MAX as u128 - 1),
                },
                Case {
                    scenario: "IPv6 /128 includes the maximum host address",
                    input: CountInput {
                        prefix: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/128",
                        num_reserved: 0,
                        gateway: None,
                        svi: None,
                        used_ips: &[],
                    },
                    expect: Yields(1),
                },
                Case {
                    scenario: "reservation past the maximum IPv6 address leaves no hosts",
                    input: CountInput {
                        prefix: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe/127",
                        num_reserved: 2,
                        gateway: None,
                        svi: None,
                        used_ips: &[],
                    },
                    expect: Yields(0),
                },
                Case {
                    scenario: "negative IPv4 reservation count remains an accounting error",
                    input: CountInput {
                        prefix: "192.0.2.0/24",
                        num_reserved: -1,
                        gateway: None,
                        svi: None,
                        used_ips: &[],
                    },
                    expect: Fails,
                },
            ],
            |input| {
                let prefix = NetworkPrefix {
                    id: NetworkPrefixId::new(),
                    segment_id: NetworkSegmentId::new(),
                    prefix: input.prefix.parse().unwrap(),
                    gateway: input.gateway.map(|address| address.parse().unwrap()),
                    dhcpv6_link_address: None,
                    num_reserved: input.num_reserved,
                    vpc_prefix_id: None,
                    vpc_prefix: None,
                    svi_ip: input.svi.map(|address| address.parse().unwrap()),
                    num_free_ips: None,
                };
                let used_ips = input
                    .used_ips
                    .iter()
                    .map(|address| address.parse().unwrap())
                    .collect::<Vec<_>>();
                super::super::num_free_host_addresses(&prefix, &used_ips).map_err(|error| {
                    assert!(
                        matches!(
                            &error,
                            DatabaseError::Internal { message }
                                if message.contains("negative reservation count -1")
                        ),
                        "{error:?}"
                    );
                })
            },
        );
    }

    #[test]
    fn finds_ordered_gaps_without_enumerating_address_space() {
        check_cases(
            [
                Case {
                    scenario: "IPv4 gaps skip gateway, SVI, and occupied addresses",
                    input: Input {
                        prefix: "192.0.2.0/29",
                        num_reserved: 0,
                        gateway: Some("192.0.2.5"),
                        svi: Some("192.0.2.3"),
                        occupied: &[("192.0.2.2", Some("192.0.2.5")), ("192.0.2.6", None)],
                        limit: 32,
                    },
                    expect: Yields(vec!["192.0.2.1".to_string(), "192.0.2.4".to_string()]),
                },
                Case {
                    scenario: "IPv4 without a gateway still reserves the second address",
                    input: Input {
                        prefix: "192.0.2.0/30",
                        num_reserved: 0,
                        gateway: None,
                        svi: None,
                        occupied: &[],
                        limit: 32,
                    },
                    expect: Yields(vec!["192.0.2.2".to_string()]),
                },
                Case {
                    scenario: "IPv4 /31 has no usable addresses under the host reservation policy",
                    input: Input {
                        prefix: "192.0.2.0/31",
                        num_reserved: 0,
                        gateway: Some("192.0.2.1"),
                        svi: None,
                        occupied: &[],
                        limit: 32,
                    },
                    expect: Yields(vec![]),
                },
                Case {
                    scenario: "IPv6 /0 skips a large reservation range and limits the batch",
                    input: Input {
                        prefix: "::/0",
                        num_reserved: i32::MAX,
                        gateway: None,
                        svi: None,
                        occupied: &[("::7fff:ffff", None)],
                        limit: 2,
                    },
                    expect: Yields(vec!["::8000:0".to_string(), "::8000:1".to_string()]),
                },
                Case {
                    scenario: "IPv6 /126 excludes endpoints and an otherwise free SVI",
                    input: Input {
                        prefix: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffc/126",
                        num_reserved: 0,
                        gateway: None,
                        svi: Some("ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe"),
                        occupied: &[],
                        limit: 32,
                    },
                    expect: Yields(vec!["ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffd".to_string()]),
                },
                Case {
                    scenario: "IPv6 /127 includes both endpoints at the address-space boundary",
                    input: Input {
                        prefix: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe/127",
                        num_reserved: 0,
                        gateway: None,
                        svi: None,
                        occupied: &[],
                        limit: 32,
                    },
                    expect: Yields(vec![
                        "ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe".to_string(),
                        "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff".to_string(),
                    ]),
                },
                Case {
                    scenario: "occupied maximum IPv6 address exhausts the prefix without wrapping",
                    input: Input {
                        prefix: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/128",
                        num_reserved: 0,
                        gateway: None,
                        svi: None,
                        occupied: &[("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", None)],
                        limit: 32,
                    },
                    expect: Yields(vec![]),
                },
                Case {
                    scenario: "reservation range covering the maximum IPv6 address exhausts the prefix",
                    input: Input {
                        prefix: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe/127",
                        num_reserved: 2,
                        gateway: None,
                        svi: None,
                        occupied: &[],
                        limit: 32,
                    },
                    expect: Yields(vec![]),
                },
                Case {
                    scenario: "invalid IPv6 reservation count remains an error",
                    input: Input {
                        prefix: "2001:db8::/64",
                        num_reserved: -1,
                        gateway: None,
                        svi: None,
                        occupied: &[],
                        limit: 32,
                    },
                    expect: Fails,
                },
            ],
            |input| {
                let prefix = NetworkPrefix {
                    id: NetworkPrefixId::new(),
                    segment_id: NetworkSegmentId::new(),
                    prefix: input.prefix.parse().unwrap(),
                    gateway: input.gateway.map(|address| address.parse().unwrap()),
                    dhcpv6_link_address: None,
                    num_reserved: input.num_reserved,
                    vpc_prefix_id: None,
                    vpc_prefix: None,
                    svi_ip: input.svi.map(|address| address.parse().unwrap()),
                    num_free_ips: None,
                };
                let occupied = input
                    .occupied
                    .iter()
                    .map(|(address, next_address)| OccupiedAddress {
                        address: address.parse().unwrap(),
                        next_address: next_address.map(|address| address.parse().unwrap()),
                    })
                    .collect::<Vec<_>>();
                HostAddressCandidates::new(&prefix)
                    .map(|selector| {
                        selector
                            .select(&occupied, input.limit)
                            .into_iter()
                            .map(|address| address.to_string())
                            .collect::<Vec<_>>()
                    })
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn omitted_occupied_runs_and_an_unread_tail_remain_unavailable() {
        let prefix = NetworkPrefix {
            id: NetworkPrefixId::new(),
            segment_id: NetworkSegmentId::new(),
            prefix: "192.0.2.0/28".parse().unwrap(),
            gateway: Some("192.0.2.2".parse().unwrap()),
            dhcpv6_link_address: None,
            num_reserved: 0,
            vpc_prefix_id: None,
            vpc_prefix: None,
            svi_ip: Some("192.0.2.5".parse().unwrap()),
            num_free_ips: None,
        };
        // The query omits adjacent occupied rows, and its limit can omit the
        // final occupied row. Neither omission proves another address is free.
        let occupied = [
            ("192.0.2.1", "192.0.2.3"),
            ("192.0.2.4", "192.0.2.6"),
            ("192.0.2.6", "192.0.2.8"),
            ("192.0.2.8", "192.0.2.10"),
            ("192.0.2.11", "192.0.2.13"),
        ]
        .map(|(address, next_address)| OccupiedAddress {
            address: address.parse().unwrap(),
            next_address: Some(next_address.parse().unwrap()),
        });
        let candidates = HostAddressCandidates::new(&prefix)
            .unwrap()
            .select(&occupied, 32);
        assert_eq!(
            candidates,
            ["192.0.2.7", "192.0.2.9", "192.0.2.12"]
                .map(|address| address.parse::<IpAddr>().unwrap())
        );
    }

    #[test]
    fn leading_gap_threshold_allows_both_exclusions_without_wrapping() {
        struct ThresholdCase {
            cidr: &'static str,
            gateway: Option<&'static str>,
            svi: Option<&'static str>,
            expected_threshold: Option<&'static str>,
            expected_last: Option<&'static str>,
        }
        for case in [
            ThresholdCase {
                cidr: "192.0.2.0/26",
                gateway: Some("192.0.2.2"),
                svi: Some("192.0.2.3"),
                expected_threshold: Some("192.0.2.35"),
                expected_last: Some("192.0.2.34"),
            },
            ThresholdCase {
                cidr: "255.255.255.224/27",
                gateway: None,
                svi: None,
                expected_threshold: None,
                expected_last: None,
            },
            ThresholdCase {
                cidr: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe/127",
                gateway: None,
                svi: None,
                expected_threshold: None,
                expected_last: None,
            },
        ] {
            let prefix = NetworkPrefix {
                id: NetworkPrefixId::new(),
                segment_id: NetworkSegmentId::new(),
                prefix: case.cidr.parse().unwrap(),
                gateway: case.gateway.map(|address| address.parse().unwrap()),
                dhcpv6_link_address: None,
                num_reserved: 0,
                vpc_prefix_id: None,
                vpc_prefix: None,
                svi_ip: case.svi.map(|address| address.parse().unwrap()),
                num_free_ips: None,
            };
            let selector = HostAddressCandidates::new(&prefix).unwrap();
            let threshold = selector.leading_gap_threshold(32);
            assert_eq!(
                threshold,
                case.expected_threshold
                    .map(|address| address.parse::<IpAddr>().unwrap()),
                "{}",
                case.cidr
            );
            if let Some(address) = threshold {
                let candidates = selector.select(
                    &[OccupiedAddress {
                        address,
                        next_address: None,
                    }],
                    32,
                );
                assert_eq!(candidates.len(), 32);
                assert_eq!(
                    candidates.last().copied(),
                    case.expected_last
                        .map(|address| address.parse::<IpAddr>().unwrap())
                );
            }
        }
    }

    #[test]
    fn advancing_past_candidates_preserves_family_bounds() {
        #[derive(Debug)]
        struct Input {
            cidr: &'static str,
            consumed: &'static str,
        }

        #[derive(Debug, PartialEq)]
        struct Output {
            range: Option<(IpAddr, IpAddr)>,
            leading_gap_threshold: Option<IpAddr>,
            candidates: Vec<IpAddr>,
        }

        check_values(
            [
                Check {
                    scenario: "next batch moves the query bounds and leading-gap threshold",
                    input: Input {
                        cidr: "192.0.2.0/24",
                        consumed: "192.0.2.33",
                    },
                    expect: Output {
                        range: Some((
                            "192.0.2.34".parse().unwrap(),
                            "192.0.2.254".parse().unwrap(),
                        )),
                        leading_gap_threshold: Some("192.0.2.68".parse().unwrap()),
                        candidates: vec!["192.0.2.34".parse().unwrap()],
                    },
                },
                Check {
                    scenario: "IPv4 stops before the broadcast address",
                    input: Input {
                        cidr: "255.255.255.252/30",
                        consumed: "255.255.255.254",
                    },
                    expect: Output {
                        range: None,
                        leading_gap_threshold: None,
                        candidates: vec![],
                    },
                },
                Check {
                    scenario: "IPv6 /0 advances across the low 64-bit boundary",
                    input: Input {
                        cidr: "::/0",
                        consumed: "::ffff:ffff:ffff:ffff",
                    },
                    expect: Output {
                        range: Some((
                            "0:0:0:1::".parse().unwrap(),
                            "ffff:ffff:ffff:ffff:ffff:ffff:ffff:fffe".parse().unwrap(),
                        )),
                        leading_gap_threshold: Some("0:0:0:1::22".parse().unwrap()),
                        candidates: vec!["0:0:0:1::".parse().unwrap()],
                    },
                },
                Check {
                    scenario: "the maximum IPv6 candidate exhausts the cursor without wrapping",
                    input: Input {
                        cidr: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff/128",
                        consumed: "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
                    },
                    expect: Output {
                        range: None,
                        leading_gap_threshold: None,
                        candidates: vec![],
                    },
                },
            ],
            |input| {
                let prefix = NetworkPrefix {
                    id: NetworkPrefixId::new(),
                    segment_id: NetworkSegmentId::new(),
                    prefix: input.cidr.parse().unwrap(),
                    gateway: None,
                    dhcpv6_link_address: None,
                    num_reserved: 0,
                    vpc_prefix_id: None,
                    vpc_prefix: None,
                    svi_ip: None,
                    num_free_ips: None,
                };
                let mut selector = HostAddressCandidates::new(&prefix).unwrap();
                selector.advance_past(input.consumed.parse().unwrap());
                Output {
                    range: selector.range(),
                    leading_gap_threshold: selector.leading_gap_threshold(32),
                    candidates: selector.select(&[], 1),
                }
            },
        );
    }
}
