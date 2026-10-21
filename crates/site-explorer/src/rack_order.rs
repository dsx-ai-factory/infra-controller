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

//! Creation order of the hosts one Site Explorer run identified.
//!
//! A rack becomes usable once every expected host, switch, and power shelf in
//! it has a machine, so the order in which hosts are created decides when
//! racks complete. The identify phase produces hosts in hash order; this
//! module replaces that with a rack-affine order so each run finishes the
//! racks it can before it starts new ones.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use carbide_uuid::rack::RackId;

use crate::IdentifiedManagedHost;

/// Rack host fill before a run's creation phase, over racks with at least
/// one expected host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RackHostFill {
    /// Racks with at least one expected host.
    pub(crate) expected: usize,
    /// Racks whose expected hosts all have a machine.
    pub(crate) all_hosts_ingested: usize,
    /// Racks whose expected hosts still without a machine were all
    /// identified for creation in this run. The per-run creation cap may
    /// still defer some of them.
    pub(crate) completable_this_run: usize,
}

/// Sort position of a host's rack. Racks come before hosts without one.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum RackRank<'a> {
    Racked {
        /// Expected hosts of the rack still without a machine after every
        /// identified host of this run is created.
        missing_after_run: usize,
        /// Expected hosts of the rack without a machine now.
        missing_now: usize,
        rack_id: &'a str,
    },
    Unracked,
}

/// Ascending sort key of an identified host. Hosts that still need a machine
/// come first, then the racks this run completes, then the racks closest to
/// complete; one rack's hosts stay contiguous and the BMC IP settles ties.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct CreationOrderKey<'a> {
    ingested: bool,
    rack: RackRank<'a>,
    host_bmc_ip: IpAddr,
}

/// Sorts `hosts` into creation order and returns the rack fill the order was
/// computed from.
///
/// `rack_of` resolves a host BMC IP to its expected rack, and
/// `expected_hosts_per_rack` counts the expected hosts of every rack.
/// `ingested_bmc_ips` holds every BMC address that already belongs to a
/// machine; a DPU BMC address resolves to no rack, so it is not counted.
pub(crate) fn order_hosts_by_rack_completion<'a>(
    hosts: &mut [IdentifiedManagedHost],
    rack_of: impl Fn(&IpAddr) -> Option<&'a RackId>,
    expected_hosts_per_rack: &HashMap<&'a RackId, usize>,
    ingested_bmc_ips: &HashSet<IpAddr>,
) -> RackHostFill {
    let mut ingested_per_rack: HashMap<&RackId, usize> = HashMap::new();
    for bmc_ip in ingested_bmc_ips {
        if let Some(rack_id) = rack_of(bmc_ip) {
            *ingested_per_rack.entry(rack_id).or_default() += 1;
        }
    }
    let mut candidates_per_rack: HashMap<&RackId, usize> = HashMap::new();
    for host in hosts.iter() {
        let bmc_ip = &host.explored_host.host_bmc_ip;
        if !ingested_bmc_ips.contains(bmc_ip)
            && let Some(rack_id) = rack_of(bmc_ip)
        {
            *candidates_per_rack.entry(rack_id).or_default() += 1;
        }
    }

    let mut fill = RackHostFill::default();
    let mut missing_per_rack: HashMap<&RackId, (usize, usize)> = HashMap::new();
    for (rack_id, &expected) in expected_hosts_per_rack {
        let ingested = ingested_per_rack.get(rack_id).copied().unwrap_or(0);
        let candidates = candidates_per_rack.get(rack_id).copied().unwrap_or(0);
        let missing_now = expected.saturating_sub(ingested);
        let missing_after_run = missing_now.saturating_sub(candidates);
        fill.expected += 1;
        if missing_now == 0 {
            fill.all_hosts_ingested += 1;
        } else if missing_after_run == 0 {
            fill.completable_this_run += 1;
        }
        missing_per_rack.insert(rack_id, (missing_after_run, missing_now));
    }

    hosts.sort_by_cached_key(|host| {
        let host_bmc_ip = host.explored_host.host_bmc_ip;
        let rack = rack_of(&host_bmc_ip)
            .and_then(|rack_id| {
                missing_per_rack
                    .get(rack_id)
                    .map(|&(missing_after_run, missing_now)| RackRank::Racked {
                        missing_after_run,
                        missing_now,
                        rack_id: rack_id.as_str(),
                    })
            })
            .unwrap_or(RackRank::Unracked);
        CreationOrderKey {
            ingested: ingested_bmc_ips.contains(&host_bmc_ip),
            rack,
            host_bmc_ip,
        }
    });

    fill
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use model::site_explorer::{EndpointExplorationReport, ExploredManagedHost};

    use super::*;

    struct OrderCase {
        /// Identified hosts in input order, as (BMC IP, expected rack).
        hosts: Vec<(&'static str, Option<&'static str>)>,
        /// Host BMC IPs that already have a machine; those not in `hosts`
        /// are ingested hosts the run did not identify.
        ingested: Vec<(&'static str, &'static str)>,
        /// Expected host count per rack.
        expected: Vec<(&'static str, usize)>,
    }

    #[derive(Debug, PartialEq)]
    struct Ordered {
        bmc_ips: Vec<IpAddr>,
        fill: RackHostFill,
    }

    fn ips(bmc_ips: &[&str]) -> Vec<IpAddr> {
        bmc_ips
            .iter()
            .map(|bmc_ip| bmc_ip.parse().unwrap())
            .collect()
    }

    fn identified_host(bmc_ip: &str) -> IdentifiedManagedHost {
        IdentifiedManagedHost {
            explored_host: ExploredManagedHost {
                host_bmc_ip: bmc_ip.parse().unwrap(),
                dpus: Vec::new(),
            },
            report: EndpointExplorationReport::default(),
            primary_interface_selection: None,
        }
    }

    fn order(case: OrderCase) -> Ordered {
        let racks: Vec<RackId> = case
            .expected
            .iter()
            .map(|(rack_id, _)| RackId::new(*rack_id))
            .collect();
        let rack_by_name: HashMap<&str, &RackId> =
            racks.iter().map(|rack| (rack.as_str(), rack)).collect();
        let expected_hosts_per_rack: HashMap<&RackId, usize> = case
            .expected
            .iter()
            .map(|(rack_id, count)| (rack_by_name[rack_id], *count))
            .collect();
        let mut rack_of_ip: HashMap<IpAddr, &RackId> = HashMap::new();
        for (bmc_ip, rack_id) in &case.hosts {
            if let Some(rack_id) = rack_id {
                rack_of_ip.insert(bmc_ip.parse().unwrap(), rack_by_name[rack_id]);
            }
        }
        let mut ingested_bmc_ips = HashSet::new();
        for (bmc_ip, rack_id) in &case.ingested {
            let bmc_ip: IpAddr = bmc_ip.parse().unwrap();
            rack_of_ip.insert(bmc_ip, rack_by_name[rack_id]);
            ingested_bmc_ips.insert(bmc_ip);
        }
        let mut hosts: Vec<IdentifiedManagedHost> = case
            .hosts
            .iter()
            .map(|(bmc_ip, _)| identified_host(bmc_ip))
            .collect();

        let fill = order_hosts_by_rack_completion(
            &mut hosts,
            |bmc_ip| rack_of_ip.get(bmc_ip).copied(),
            &expected_hosts_per_rack,
            &ingested_bmc_ips,
        );
        Ordered {
            bmc_ips: hosts
                .iter()
                .map(|host| host.explored_host.host_bmc_ip)
                .collect(),
            fill,
        }
    }

    #[test]
    fn creation_order_cases() {
        check_values(
            [
                Check {
                    scenario: "a rack this run completes sorts before one it cannot",
                    input: OrderCase {
                        hosts: vec![
                            ("10.0.0.21", Some("rack-b")),
                            ("10.0.0.11", Some("rack-a")),
                            ("10.0.0.22", Some("rack-b")),
                            ("10.0.0.12", Some("rack-a")),
                        ],
                        ingested: vec![],
                        expected: vec![("rack-a", 2), ("rack-b", 3)],
                    },
                    expect: Ordered {
                        bmc_ips: ips(&["10.0.0.11", "10.0.0.12", "10.0.0.21", "10.0.0.22"]),
                        fill: RackHostFill {
                            expected: 2,
                            all_hosts_ingested: 0,
                            completable_this_run: 1,
                        },
                    },
                },
                Check {
                    scenario: "among completable racks the one with fewer hosts to create goes first",
                    input: OrderCase {
                        hosts: vec![
                            ("10.0.0.21", Some("rack-b")),
                            ("10.0.0.22", Some("rack-b")),
                            ("10.0.0.13", Some("rack-a")),
                        ],
                        ingested: vec![("10.0.0.11", "rack-a"), ("10.0.0.12", "rack-a")],
                        expected: vec![("rack-a", 3), ("rack-b", 2)],
                    },
                    expect: Ordered {
                        bmc_ips: ips(&["10.0.0.13", "10.0.0.21", "10.0.0.22"]),
                        fill: RackHostFill {
                            expected: 2,
                            all_hosts_ingested: 0,
                            completable_this_run: 2,
                        },
                    },
                },
                Check {
                    scenario: "equal racks stay contiguous in rack id order",
                    input: OrderCase {
                        hosts: vec![
                            ("10.0.0.21", Some("rack-b")),
                            ("10.0.0.11", Some("rack-a")),
                            ("10.0.0.22", Some("rack-b")),
                            ("10.0.0.12", Some("rack-a")),
                        ],
                        ingested: vec![],
                        expected: vec![("rack-a", 4), ("rack-b", 4)],
                    },
                    expect: Ordered {
                        bmc_ips: ips(&["10.0.0.11", "10.0.0.12", "10.0.0.21", "10.0.0.22"]),
                        fill: RackHostFill {
                            expected: 2,
                            all_hosts_ingested: 0,
                            completable_this_run: 0,
                        },
                    },
                },
                Check {
                    scenario: "hosts without an expected rack sort last by BMC IP",
                    input: OrderCase {
                        hosts: vec![
                            ("10.0.0.9", None),
                            ("10.0.0.11", Some("rack-a")),
                            ("10.0.0.3", None),
                        ],
                        ingested: vec![],
                        expected: vec![("rack-a", 1)],
                    },
                    expect: Ordered {
                        bmc_ips: ips(&["10.0.0.11", "10.0.0.3", "10.0.0.9"]),
                        fill: RackHostFill {
                            expected: 1,
                            all_hosts_ingested: 0,
                            completable_this_run: 1,
                        },
                    },
                },
                Check {
                    scenario: "a host that already has a machine sorts after every host to create",
                    input: OrderCase {
                        hosts: vec![("10.0.0.11", Some("rack-a")), ("10.0.0.21", Some("rack-b"))],
                        ingested: vec![("10.0.0.11", "rack-a")],
                        expected: vec![("rack-a", 1), ("rack-b", 1)],
                    },
                    expect: Ordered {
                        bmc_ips: ips(&["10.0.0.21", "10.0.0.11"]),
                        fill: RackHostFill {
                            expected: 2,
                            all_hosts_ingested: 1,
                            completable_this_run: 1,
                        },
                    },
                },
                Check {
                    scenario: "an identified host that has a machine is not a candidate of its rack",
                    input: OrderCase {
                        hosts: vec![
                            ("10.0.0.11", Some("rack-a")),
                            ("10.0.0.12", Some("rack-a")),
                            ("10.0.0.21", Some("rack-b")),
                        ],
                        ingested: vec![("10.0.0.11", "rack-a")],
                        expected: vec![("rack-a", 3), ("rack-b", 1)],
                    },
                    expect: Ordered {
                        bmc_ips: ips(&["10.0.0.21", "10.0.0.12", "10.0.0.11"]),
                        fill: RackHostFill {
                            expected: 2,
                            all_hosts_ingested: 0,
                            completable_this_run: 1,
                        },
                    },
                },
                Check {
                    scenario: "ties within a rack settle on the BMC IP",
                    input: OrderCase {
                        hosts: vec![("10.0.0.12", Some("rack-a")), ("10.0.0.11", Some("rack-a"))],
                        ingested: vec![],
                        expected: vec![("rack-a", 2)],
                    },
                    expect: Ordered {
                        bmc_ips: ips(&["10.0.0.11", "10.0.0.12"]),
                        fill: RackHostFill {
                            expected: 1,
                            all_hosts_ingested: 0,
                            completable_this_run: 1,
                        },
                    },
                },
                Check {
                    scenario: "no identified hosts still reports the rack fill",
                    input: OrderCase {
                        hosts: vec![],
                        ingested: vec![("10.0.0.11", "rack-a")],
                        expected: vec![("rack-a", 1), ("rack-b", 2)],
                    },
                    expect: Ordered {
                        bmc_ips: ips(&[]),
                        fill: RackHostFill {
                            expected: 2,
                            all_hosts_ingested: 1,
                            completable_this_run: 0,
                        },
                    },
                },
            ],
            order,
        );
    }
}
