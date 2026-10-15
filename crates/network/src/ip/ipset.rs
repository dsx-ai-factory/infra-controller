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
use std::collections::BTreeSet;
use std::ops::RangeInclusive;

use crate::ip::prefix::{IpPrefix, Ipv4Prefix, Ipv6Prefix, ToPrefix};

/// An IpSet is a specialized set-type data structure for IP addresses, which
/// internally is represented as a set of prefixes that cover the included
/// address space.
pub struct IpSet {
    // Anything updating this set must maintain these invariants:
    // 1. The prefixes are disjoint (not overlapping)
    // 2. No adjacent sibling prefixes exist; these must be stored as their
    //    equivalent parent.
    included_prefixes: BTreeSet<IpPrefix>,
}

impl IpSet {
    /// Return whether the specified value is included in the set. The value can
    /// be an IpPrefix, an IpAddr, or anything else that implements ToPrefix.
    pub fn contains<P: ToPrefix>(&self, value: P) -> bool {
        let prefix = value.to_prefix();
        self.contains_prefix(&prefix)
    }

    fn contains_prefix(&self, prefix: &IpPrefix) -> bool {
        self.get_containing_prefix(prefix).is_some()
    }

    fn get_containing_prefix(&self, prefix: &IpPrefix) -> Option<IpPrefix> {
        self.included_prefixes
            .range(..=prefix)
            .last()
            .and_then(|included| included.contains(prefix).then_some(*included))
    }

    /// Add a prefix to the included set. If the set already contains the address
    /// space in the prefix, this is a no-op.
    pub fn add(&mut self, prefix: IpPrefix) {
        if self.contains_prefix(&prefix) {
            return;
        }

        // Remove all smaller subprefixes contained by what we're
        // about to insert.
        while let Some(subprefix) = self
            .included_prefixes
            .range(subprefix_range(prefix))
            .find_map(|p| prefix.contains(p).then_some(*p))
        {
            self.included_prefixes.remove(&subprefix);
        }

        // Before inserting this prefix, look for its sibling and try to
        // aggregate with it (and then check for a sibling of the new aggregate,
        // and so on recursively).
        let mut prefix = prefix;
        while let Some(sibling) = prefix
            .get_sibling()
            .and_then(|sibling| self.included_prefixes.take(&sibling))
        {
            // We already know these are siblings, and therefore don't expect
            // this .try_aggregate() call to fail.
            let aggregated = prefix.try_aggregate(&sibling).unwrap();
            prefix = aggregated;
        }
        self.included_prefixes.insert(prefix);
    }

    /// Remove the address space represented by this prefix from the set.
    pub fn remove(&mut self, prefix: &IpPrefix) {
        let container = match self.get_containing_prefix(prefix) {
            Some(included) => included,
            None => {
                // If we didn't find a containing prefix to work on, the other
                // possibility we need to account for is one or more smaller
                // prefixes that are contained by the prefix we're removing.
                while let Some(subprefix) = self
                    .included_prefixes
                    .range(subprefix_range(*prefix))
                    .find_map(|included| prefix.contains(included).then_some(*included))
                {
                    self.included_prefixes.remove(&subprefix);
                }
                return;
            }
        };
        self.included_prefixes.remove(&container);

        // The prefix we removed may have been a superset of what we were asked
        // to remove, so let's recursively bifurcate/fragment it until we're at
        // the size requested. The non-matching fragments will be re-inserted
        // into the set.
        let mut container = container;
        while container != *prefix {
            container = match container.bifurcate().unwrap() {
                (c1, c2) if c1.contains(prefix) => {
                    self.included_prefixes.insert(c2);
                    c1
                }
                (c1, c2) if c2.contains(prefix) => {
                    self.included_prefixes.insert(c1);
                    c2
                }
                _ => unreachable!(),
            }
        }
    }

    /// Get the whole included address space as a list of aggregate prefixes.
    pub fn get_prefixes(&self) -> Vec<IpPrefix> {
        self.included_prefixes.iter().copied().collect()
    }

    /// Get just the IPv4 address space as a list of aggregate prefixes.
    pub fn get_ipv4_prefixes(&self) -> Vec<Ipv4Prefix> {
        self.included_prefixes
            .iter()
            .filter_map(|prefix| match prefix {
                IpPrefix::V4(ipv4_prefix) => Some(*ipv4_prefix),
                _ => None,
            })
            .collect()
    }

    /// Get just the IPv6 address space as a list of aggregate prefixes.
    pub fn get_ipv6_prefixes(&self) -> Vec<Ipv6Prefix> {
        self.included_prefixes
            .iter()
            .filter_map(|prefix| match prefix {
                IpPrefix::V6(ipv6_prefix) => Some(*ipv6_prefix),
                _ => None,
            })
            .collect()
    }

    /// Create a new set with nothing contained.
    pub fn new_empty() -> Self {
        Self {
            included_prefixes: BTreeSet::new(),
        }
    }
}

// Return a range that spans all of the possible subprefixes this prefix
// could contain.
//
// This functionality relies on the specific Ord implementation of
// `IpPrefix`.
fn subprefix_range(prefix: IpPrefix) -> RangeInclusive<IpPrefix> {
    prefix..=prefix.get_last_subprefix()
}

impl From<IpPrefix> for IpSet {
    fn from(value: IpPrefix) -> Self {
        let included_prefixes = BTreeSet::from([value]);
        Self { included_prefixes }
    }
}

impl<I> From<I> for IpSet
where
    I: IntoIterator<Item: ToPrefix>,
{
    fn from(value: I) -> Self {
        let mut ipset = Self::new_empty();
        let prefixes = value.into_iter();
        prefixes.for_each(|p| ipset.add(p.to_prefix()));
        ipset
    }
}

/// Given an iterator over prefix-like sources, return a list of prefixes
/// that cover all of the address space after merging adjacent prefixes and
/// deduplicating. This is a convenience function for constructing an IpSet
/// and getting its resulting prefixes.
pub fn aggregate_prefixes<I>(prefix_sources: I) -> Vec<IpPrefix>
where
    I: IntoIterator<Item: ToPrefix>,
{
    let ipset = IpSet::from(prefix_sources);
    ipset.get_prefixes()
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::str::FromStr;

    use carbide_test_support::{Check, check_values, value_scenarios};

    use super::*;

    /// Parse a prefix string into an `IpPrefix`, panicking on malformed input.
    /// All the strings below are test literals known to be valid.
    fn pfx(s: &str) -> IpPrefix {
        IpPrefix::from_str(s).unwrap_or_else(|e| panic!("bad test prefix {s:?}: {e}"))
    }

    /// Parse a slice of prefix strings into owned `IpPrefix` values.
    fn pfxs(strs: &[&str]) -> Vec<IpPrefix> {
        strs.iter().map(|s| pfx(s)).collect()
    }

    /// Build an `IpSet` by adding each of the given prefix strings, then return
    /// its aggregate prefixes as strings for easy table comparison.
    fn add_all_and_dump(strs: &[&str]) -> Vec<String> {
        let mut ipset = IpSet::new_empty();
        for s in strs {
            ipset.add(pfx(s));
        }
        ipset.get_prefixes().iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn membership_covers_set_and_gaps() {
        // The set is the whole 10/8 net; check addresses and prefixes inside it,
        // on each boundary, and just outside on either side.
        let ipset = IpSet::from(pfx("10.0.0.0/8"));
        value_scenarios!(
            run = |s| ipset.contains(pfx(s));
            "the defining prefix itself" {
                "10.0.0.0/8" => true,
            }

            "first address in range" {
                "10.0.0.0/32" => true,
            }

            "last address in range" {
                "10.255.255.255/32" => true,
            }

            "an interior subprefix" {
                "10.128.0.0/9" => true,
            }

            "a deep interior address" {
                "10.1.2.3/32" => true,
            }

            "one address before the range" {
                "9.255.255.255/32" => false,
            }

            "one address after the range" {
                "11.0.0.0/32" => false,
            }

            "a wider prefix that is not contained" {
                "10.0.0.0/7" => false,
            }

            "an unrelated v4 prefix" {
                "192.168.0.0/16" => false,
            }

            "an IPv6 prefix is never in a v4-only set" {
                "2001:db8::/32" => false,
            }
        );
    }

    #[test]
    fn membership_of_the_empty_set_is_always_false() {
        let ipset = IpSet::new_empty();
        value_scenarios!(
            run = |s| ipset.contains(pfx(s));
            "v4 address" {
                "10.0.0.1/32" => false,
            }

            "v4 prefix" {
                "0.0.0.0/0" => false,
            }

            "v6 prefix" {
                "::/0" => false,
            }
        );
    }

    #[test]
    fn membership_of_disjoint_v4_v6_set() {
        // A set holding both a v4 and a v6 prefix; each family answers
        // independently and never crosses over.
        let ipset = IpSet::from([pfx("10.0.0.0/8"), pfx("2001:db8::/32")]);
        value_scenarios!(
            run = |s| ipset.contains(pfx(s));
            "inside the v4 member" {
                "10.5.5.5/32" => true,
            }

            "inside the v6 member" {
                "2001:db8:abcd::/48" => true,
            }

            "outside the v4 member" {
                "11.0.0.0/8" => false,
            }

            "outside the v6 member" {
                "2001:db9::/32" => false,
            }
        );
    }

    #[test]
    fn adding_prefixes_aggregates_and_dedups() {
        // Each row builds a fresh set from the given inputs and asserts the
        // resulting aggregate prefixes. Order of insertion must not matter.
        value_scenarios!(
            run = add_all_and_dump;
            "single prefix is stored verbatim" {
                &["10.0.0.0/24"][..] => vec!["10.0.0.0/24".to_string()],
            }

            "exact duplicate is a no-op" {
                &["10.0.0.0/24", "10.0.0.0/24"][..] => vec!["10.0.0.0/24".to_string()],
            }

            "a subprefix already covered is absorbed" {
                &["10.0.0.0/8", "10.1.2.0/24"][..] => vec!["10.0.0.0/8".to_string()],
            }

            "a superprefix swallows an existing entry" {
                &["10.1.2.0/24", "10.0.0.0/8"][..] => vec!["10.0.0.0/8".to_string()],
            }

            "two siblings aggregate into their supernet" {
                &["10.0.0.0/24", "10.0.1.0/24"][..] => vec!["10.0.0.0/23".to_string()],
            }

            "siblings aggregate regardless of insertion order" {
                &["10.0.1.0/24", "10.0.0.0/24"][..] => vec!["10.0.0.0/23".to_string()],
            }

            "non-sibling neighbors stay separate" {
                &["10.0.1.0/24", "10.0.2.0/24"][..] => vec!["10.0.1.0/24".to_string(), "10.0.2.0/24".to_string()],
            }

            "cascading aggregation up several levels" {
                &[
                    "10.0.0.0/26",
                    "10.0.0.64/26",
                    "10.0.0.128/26",
                    "10.0.0.192/26",
                ][..] => vec!["10.0.0.0/24".to_string()],
            }

            "filling a /24 from a /25 plus power-of-two pieces" {
                &[
                    "10.0.1.4/30",
                    "10.0.1.8/29",
                    "10.0.1.16/28",
                    "10.0.1.32/27",
                    "10.0.1.64/26",
                    "10.0.1.128/25",
                    "10.0.0.0/24",
                    "10.0.1.0/30",
                ][..] => vec!["10.0.0.0/23".to_string()],
            }

            "a v4 and a v6 prefix coexist, v4 sorts first" {
                &["2001:db8::/32", "10.0.0.0/8"][..] => vec!["10.0.0.0/8".to_string(), "2001:db8::/32".to_string()],
            }

            "v6 siblings aggregate too" {
                &["2001:db8:0000::/34", "2001:db8:4000::/34"][..] => vec!["2001:db8::/33".to_string()],
            }

            "a default route swallows everything in its family" {
                &["0.0.0.0/0", "10.0.0.0/8", "192.168.0.0/16"][..] => vec!["0.0.0.0/0".to_string()],
            }
        );
    }

    #[test]
    fn auto_aggregation_collapses_a_full_supernet() {
        // The original `test_auto_aggregation`: a /24 plus the pieces of its
        // sibling /24 collapse to a single /23.
        let mut ipset = IpSet::from(pfx("10.0.0.0/24"));
        for s in [
            "10.0.1.4/30",
            "10.0.1.8/29",
            "10.0.1.16/28",
            "10.0.1.32/27",
            "10.0.1.64/26",
            "10.0.1.128/25",
            "10.0.1.0/30",
        ] {
            ipset.add(pfx(s));
        }
        ipset.add(pfx("10.0.1.0/24"));
        assert_eq!(ipset.get_prefixes(), pfxs(&["10.0.0.0/23"]));
    }

    #[test]
    fn remove_outcomes() {
        // Each row starts from a set built from the first slice, removes each
        // requested prefix in order, then checks the final aggregate prefixes.
        struct Case {
            scenario: &'static str,
            start: &'static [&'static str],
            remove: &'static [&'static str],
            expect: &'static [&'static str],
        }
        let cases = [
            Case {
                scenario: "removing a member empties the set",
                start: &["10.0.0.0/24"],
                remove: &["10.0.0.0/24"],
                expect: &[],
            },
            Case {
                scenario: "removing something absent is a no-op",
                start: &["10.0.0.0/24"],
                remove: &["192.168.0.0/24"],
                expect: &["10.0.0.0/24"],
            },
            Case {
                scenario: "removing from the empty set is a no-op",
                start: &[],
                remove: &["10.0.0.0/24"],
                expect: &[],
            },
            Case {
                scenario: "removing a half leaves the other half",
                start: &["10.0.0.0/23"],
                remove: &["10.0.0.0/24"],
                expect: &["10.0.1.0/24"],
            },
            Case {
                scenario: "removing the high half leaves the low half",
                start: &["10.0.0.0/23"],
                remove: &["10.0.1.0/24"],
                expect: &["10.0.0.0/24"],
            },
            Case {
                scenario: "removing the last address leaves a staircase of fragments",
                start: &["10.0.0.0/24"],
                remove: &["10.0.0.255/32"],
                expect: &[
                    "10.0.0.0/25",
                    "10.0.0.128/26",
                    "10.0.0.192/27",
                    "10.0.0.224/28",
                    "10.0.0.240/29",
                    "10.0.0.248/30",
                    "10.0.0.252/31",
                    "10.0.0.254/32",
                ],
            },
            Case {
                scenario: "removing one member leaves disjoint others untouched",
                start: &["10.0.0.0/24", "192.168.0.0/24"],
                remove: &["10.0.0.0/24"],
                expect: &["192.168.0.0/24"],
            },
            Case {
                scenario: "a superset removal deletes all of its sparse sub-prefixes",
                start: &[
                    "10.0.0.0/24",
                    "10.0.2.0/24",
                    "10.0.4.0/24",
                    "10.0.5.0/24",
                    "192.168.0.0/16",
                ],
                remove: &["10.0.0.0/22"],
                expect: &["10.0.4.0/23", "192.168.0.0/16"],
            },
            Case {
                scenario: "removing the v6 root leaves the v4 member",
                start: &["10.0.0.0/8", "2001:db8::/32"],
                remove: &["::/0"],
                expect: &["10.0.0.0/8"],
            },
            Case {
                scenario: "removing both address family roots empties the set",
                start: &["10.0.0.0/8", "2001:db8::/32"],
                remove: &["0.0.0.0/0", "::/0"],
                expect: &[],
            },
        ];

        fn apply_remove((start, deletions): (&[&str], &[&str])) -> Vec<IpPrefix> {
            let mut ipset = IpSet::from(pfxs(start));
            for prefix in deletions {
                ipset.remove(&pfx(prefix));
            }
            ipset.get_prefixes()
        }

        check_values(
            cases.map(|case| Check {
                scenario: case.scenario,
                input: (case.start, case.remove),
                expect: pfxs(case.expect),
            }),
            apply_remove,
        );
    }

    #[test]
    fn family_filtered_getters_split_by_address_family() {
        // get_ipv4_prefixes / get_ipv6_prefixes each keep only their family;
        // get_prefixes keeps both. Project each set to (v4 strings, v6 strings,
        // total len) so the mixed, single-family, and empty cases are all rows.
        let project = |ipset: IpSet| {
            let v4: Vec<String> = ipset
                .get_ipv4_prefixes()
                .iter()
                .map(|p| p.to_string())
                .collect();
            let v6: Vec<String> = ipset
                .get_ipv6_prefixes()
                .iter()
                .map(|p| p.to_string())
                .collect();
            (v4, v6, ipset.get_prefixes().len())
        };
        value_scenarios!(
            run = project;
            "mixed set splits into its two families" {
                IpSet::from([
                    pfx("10.0.0.0/8"),
                    pfx("192.168.0.0/16"),
                    pfx("2001:db8::/32"),
                    pfx("fd00::/8"),
                ]) => (
                    vec!["10.0.0.0/8".to_string(), "192.168.0.0/16".to_string()],
                    vec!["2001:db8::/32".to_string(), "fd00::/8".to_string()],
                    4,
                ),
            }

            "v4-only set yields nothing from the v6 getter" {
                IpSet::from(pfx("10.0.0.0/8")) => (vec!["10.0.0.0/8".to_string()], vec![], 1),
            }

            "v6-only set yields nothing from the v4 getter" {
                IpSet::from(pfx("2001:db8::/32")) => (vec![], vec!["2001:db8::/32".to_string()], 1),
            }

            "empty set yields nothing from either getter" {
                IpSet::new_empty() => (vec![], vec![], 0),
            }
        );
    }

    #[test]
    fn from_iterator_constructs_an_aggregated_set() {
        // The `From<IntoIterator>` impl runs every item through `add`, so the
        // result is already aggregated and deduplicated.
        value_scenarios!(
            run = |strs: &[&str]| {
                let ipset = IpSet::from(pfxs(strs));
                ipset.get_prefixes().iter().map(|p| p.to_string()).collect()
            };
            "empty iterator yields the empty set" {
                &[][..] => Vec::<String>::new(),
            }

            "duplicates collapse" {
                &["10.0.0.0/24", "10.0.0.0/24", "10.0.0.0/24"][..] => vec!["10.0.0.0/24".to_string()],
            }

            "siblings aggregate" {
                &["10.0.0.0/24", "10.0.1.0/24"][..] => vec!["10.0.0.0/23".to_string()],
            }
        );
    }

    #[test]
    fn bulk_address_aggregation_merges_a_full_run() {
        // The original `test_bulk_address_aggregation`: every /32 from 10.0.0.0
        // up to (excluding) 10.1.0.0 covers exactly 10.0.0.0/16.
        let start_addr = Ipv4Addr::from_str("10.0.0.0").unwrap();
        let end_addr = Ipv4Addr::from_str("10.1.0.0").unwrap();
        let mut prefixes: Vec<_> = (start_addr..end_addr).map(|a| a.to_prefix()).collect();
        prefixes.as_mut_slice().reverse();
        let ipset = IpSet::from(prefixes.as_slice());
        assert_eq!(ipset.get_prefixes(), pfxs(&["10.0.0.0/16"]));
    }

    #[test]
    fn aggregate_prefixes_merges_and_sorts() {
        // The original `test_aggregate_prefixes`, generalized: each row feeds a
        // (deliberately scrambled) list through the free `aggregate_prefixes`
        // helper and asserts the merged, v4-before-v6 result.
        value_scenarios!(
            run = |strs: &[&str]| {
                aggregate_prefixes(pfxs(strs))
                    .iter()
                    .map(|p| p.to_string())
                    .collect()
            };
            "empty input yields nothing" {
                &[][..] => Vec::<String>::new(),
            }

            "a single prefix passes through" {
                &["10.0.0.0/24"][..] => vec!["10.0.0.0/24".to_string()],
            }

            "mixed v4 and v6 merge within each family and sort" {
                &[
                    "2001:db8:8000::/33",
                    "2001:db8:4000::/34",
                    "2001:db8:0000::/34",
                    "10.0.2.0/23",
                    "10.0.1.0/24",
                    "10.0.0.0/24",
                ][..] => vec!["10.0.0.0/22".to_string(), "2001:db8::/32".to_string()],
            }
        );
    }
}
