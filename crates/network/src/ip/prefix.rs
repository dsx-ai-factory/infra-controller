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
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt::Display;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use ipnet::{AddrParseError, PrefixLenError};
// These are part of our public API because of the conversion traits.
pub use ipnet::{IpNet, Ipv4Net, Ipv6Net};
#[cfg(feature = "ipnetwork")]
pub use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};

use super::address_family::{IdentifyAddressFamily, IpAddressFamily};

//
// Type definitions
//

/// This is a type that represents an IP prefix, which matches 0 or more leading
/// address bits with the remainder being unspecified or "don't-care". This
/// type uses the ipnet network types internally, but is stricter on what can be
/// parsed and stored. Here, we require that all bits after the prefix are set
/// to zero, so that there's no way to confuse this with an network interface
/// address (which has the same general representation but has a different
/// usage).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpPrefix {
    V4(Ipv4Prefix),
    V6(Ipv6Prefix),
}

impl IdentifyAddressFamily for IpPrefix {
    fn address_family(&self) -> IpAddressFamily {
        match self {
            IpPrefix::V4(_) => IpAddressFamily::Ipv4,
            IpPrefix::V6(_) => IpAddressFamily::Ipv6,
        }
    }
}

impl IpPrefix {
    pub fn contains<P: ToPrefix>(&self, other: P) -> bool {
        let other = other.to_prefix();
        use IpPrefix::*;
        match (self, &other) {
            (V4(prefix), V4(other_prefix)) => prefix.contains(other_prefix),
            (V6(prefix), V6(other_prefix)) => prefix.contains(other_prefix),
            _ => false,
        }
    }

    pub fn get_sibling(&self) -> Option<Self> {
        use IpPrefix::*;
        match self {
            V4(ipv4_prefix) => ipv4_prefix.get_sibling().map(V4),
            V6(ipv6_prefix) => ipv6_prefix.get_sibling().map(V6),
        }
    }

    pub fn bifurcate(&self) -> Option<(Self, Self)> {
        use IpPrefix::*;
        match self {
            V4(ipv4_prefix) => ipv4_prefix
                .bifurcate()
                .map(|(even, odd)| (V4(even), V4(odd))),
            V6(ipv6_prefix) => ipv6_prefix
                .bifurcate()
                .map(|(even, odd)| (V6(even), V6(odd))),
        }
    }

    pub fn get_last_subprefix(&self) -> Self {
        use IpPrefix::*;
        match self {
            V4(ipv4_prefix) => V4(ipv4_prefix.get_last_subprefix()),
            V6(ipv6_prefix) => V6(ipv6_prefix.get_last_subprefix()),
        }
    }

    pub fn try_aggregate(&self, other: &Self) -> Option<Self> {
        use IpPrefix::*;
        match (self, other) {
            (V4(p1), V4(p2)) => p1.try_aggregate(p2).map(V4),
            (V6(p1), V6(p2)) => p1.try_aggregate(p2).map(V6),
            _ => None,
        }
    }
}

/// A representation of an IPv4 prefix. The bits after the end of the length of
/// the prefix are guaranteed to be zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct Ipv4Prefix {
    prefix: Ipv4Net,
}

impl Ipv4Prefix {
    pub fn contains(&self, other: &Self) -> bool {
        self.prefix.contains(&other.prefix)
    }

    pub fn get_sibling(&self) -> Option<Self> {
        let prefix_length = self.prefix.prefix_len();
        match prefix_length {
            0 => None,
            n @ (1..=32) => {
                // We just need to flip the last prefix bit.
                let addr = self.prefix.addr();
                let addr_bits = addr.to_bits();
                let shift_amount = 32 - n;
                let single_bit_flip = 0x1u32 << shift_amount;
                let sibling_addr_bits = addr_bits ^ single_bit_flip;
                let sibling_addr = Ipv4Addr::from_bits(sibling_addr_bits);
                let sibling_prefix = Ipv4Net::new_assert(sibling_addr, prefix_length);
                Some(Self {
                    prefix: sibling_prefix,
                })
            }
            _ => unreachable!(),
        }
    }

    /// Attempt to split this prefix into the more specific prefixes that cover
    /// the same total address space. Returns None if `self` is a /32.
    pub fn bifurcate(&self) -> Option<(Self, Self)> {
        let prefix_length = self.prefix.prefix_len();
        match prefix_length {
            n @ (0..=31) => {
                // One of the returned outputs will be the same address
                // with the prefix one longer, but the other (the "odd" branch)
                // needs to have a bit flipped to 1 first.
                let addr_bits = self.prefix.addr().to_bits();
                let single_bit_flip = 0x80_00_00_00u32 >> n;
                let odd_addr_bits = addr_bits | single_bit_flip;

                let even_addr = Ipv4Addr::from_bits(addr_bits);
                let odd_addr = Ipv4Addr::from_bits(odd_addr_bits);

                let new_prefix_length = n + 1;
                let even_net = Ipv4Net::new_assert(even_addr, new_prefix_length);
                let odd_net = Ipv4Net::new_assert(odd_addr, new_prefix_length);

                let even_prefix = Self { prefix: even_net };
                let odd_prefix = Self { prefix: odd_net };
                Some((even_prefix, odd_prefix))
            }
            _ => None,
        }
    }

    /// Get the final and smallest sub-prefix of this prefix. This is equivalent
    /// to the all-ones address converted to a /32.
    pub fn get_last_subprefix(&self) -> Self {
        self.prefix.broadcast().to_v4_prefix()
    }

    pub fn try_aggregate(&self, other: &Self) -> Option<Self> {
        match (self, other, self.prefix.supernet(), other.prefix.supernet()) {
            // If one prefix contains the other, return the containing prefix.
            (p1, p2, _, _) if p1.contains(p2) => Some(*p1),
            (p1, p2, _, _) if p2.contains(p1) => Some(*p2),
            // If both prefixes have the same supernet, we can aggregate them
            // into that supernet.
            (_, _, Some(super1), Some(super2)) if super1 == super2 => Some(Self { prefix: super1 }),
            _ => None,
        }
    }

    pub fn into_inner(self) -> Ipv4Net {
        let Self { prefix } = self;
        prefix
    }
}

/// A representation of an IPv6 prefix. The bits after the end of the length of
/// the prefix are guaranteed to be zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct Ipv6Prefix {
    prefix: Ipv6Net,
}

impl Ipv6Prefix {
    pub fn contains(&self, other: &Self) -> bool {
        self.prefix.contains(&other.prefix)
    }

    pub fn get_sibling(&self) -> Option<Self> {
        let prefix_length = self.prefix.prefix_len();
        match prefix_length {
            0 => None,
            n if n <= 128 => {
                // We just need to flip the last prefix bit.
                let addr = self.prefix.addr();
                let addr_bits = addr.to_bits();
                let shift_amount = 128 - n;
                let single_bit_flip = 0x1u128 << shift_amount;
                let sibling_addr_bits = addr_bits ^ single_bit_flip;
                let sibling_addr = Ipv6Addr::from_bits(sibling_addr_bits);
                let sibling_prefix = Ipv6Net::new_assert(sibling_addr, prefix_length);
                Some(Self {
                    prefix: sibling_prefix,
                })
            }
            _ => unreachable!(),
        }
    }

    /// Attempt to split this prefix into the more specific prefixes that cover
    /// the same total address space. Returns None if `self` is a /128.
    pub fn bifurcate(&self) -> Option<(Self, Self)> {
        let prefix_length = self.prefix.prefix_len();
        match prefix_length {
            n @ (0..=127) => {
                // One of the returned outputs will be the same address
                // with the prefix one longer, but the other (the "odd" branch)
                // needs to have a bit flipped to 1 first.
                let even_addr_bits = self.prefix.addr().to_bits();
                let single_bit_flip = 0x8000_0000_0000_0000_0000_0000_0000_0000u128 >> n;
                let odd_addr_bits = even_addr_bits | single_bit_flip;

                let even_addr = Ipv6Addr::from_bits(even_addr_bits);
                let odd_addr = Ipv6Addr::from_bits(odd_addr_bits);

                let new_prefix_length = n + 1;
                let even_net = Ipv6Net::new_assert(even_addr, new_prefix_length);
                let odd_net = Ipv6Net::new_assert(odd_addr, new_prefix_length);

                let even_prefix = Self { prefix: even_net };
                let odd_prefix = Self { prefix: odd_net };
                Some((even_prefix, odd_prefix))
            }
            _ => None,
        }
    }

    /// Get the final and smallest sub-prefix of this prefix. This is equivalent
    /// to the all-ones address converted to a /128.
    pub fn get_last_subprefix(&self) -> Self {
        self.prefix.broadcast().to_v6_prefix()
    }

    pub fn try_aggregate(&self, other: &Self) -> Option<Self> {
        match (self, other, self.prefix.supernet(), other.prefix.supernet()) {
            // If one prefix contains the other, return the containing prefix.
            (p1, p2, _, _) if p1.contains(p2) => Some(*p1),
            (p1, p2, _, _) if p2.contains(p1) => Some(*p2),
            // If both prefixes have the same supernet, we can aggregate them
            // into that supernet.
            (_, _, Some(super1), Some(super2)) if super1 == super2 => Some(Self { prefix: super1 }),
            _ => None,
        }
    }

    pub fn into_inner(self) -> Ipv6Net {
        let Self { prefix } = self;
        prefix
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrefixError {
    #[error(
        "prefix not in canonical representation (address bits after prefix must be set to zero)"
    )]
    NonCanonicalRepresentation,

    #[error("parse error: {0}")]
    ParseError(#[from] AddrParseError),

    #[error("prefix length error: {0}")]
    BadPrefixLength(#[from] PrefixLenError),
}

//
// Trait definitions
//

/// Basic common operations on a prefix
pub trait Prefix {
    fn prefix_length(&self) -> usize;
}

/// ToPrefix can be implemented for something like a network or address where
/// we can create a prefix through some trivial operation like appending /32 or
/// truncating the trailing address bits.
pub trait ToPrefix {
    /// Create an IpPrefix from a source type.
    fn to_prefix(&self) -> IpPrefix;
}

pub trait ToV4Prefix {
    /// Create an Ipv4Prefix from a source type.
    fn to_v4_prefix(&self) -> Ipv4Prefix;
}

pub trait ToV6Prefix {
    /// Create an Ipv6Prefix from a source type.
    fn to_v6_prefix(&self) -> Ipv6Prefix;
}

//
// Functions
//

pub use super::ipset::aggregate_prefixes as aggregate;

//
// Implementations of our traits
//

impl Prefix for Ipv4Prefix {
    fn prefix_length(&self) -> usize {
        self.prefix.prefix_len() as usize
    }
}

impl Prefix for Ipv6Prefix {
    fn prefix_length(&self) -> usize {
        self.prefix.prefix_len() as usize
    }
}

impl Prefix for IpPrefix {
    fn prefix_length(&self) -> usize {
        match self {
            IpPrefix::V4(v4) => v4.prefix_length(),
            IpPrefix::V6(v6) => v6.prefix_length(),
        }
    }
}

impl<B> ToPrefix for B
where
    B: Borrow<IpPrefix>,
{
    fn to_prefix(&self) -> IpPrefix {
        *self.borrow()
    }
}

impl ToPrefix for Ipv4Prefix {
    fn to_prefix(&self) -> IpPrefix {
        IpPrefix::V4(*self)
    }
}

impl ToPrefix for Ipv6Prefix {
    fn to_prefix(&self) -> IpPrefix {
        IpPrefix::V6(*self)
    }
}

impl ToPrefix for IpAddr {
    fn to_prefix(&self) -> IpPrefix {
        match self {
            IpAddr::V4(ipv4_addr) => IpPrefix::V4(ipv4_addr.to_v4_prefix()),
            IpAddr::V6(ipv6_addr) => IpPrefix::V6(ipv6_addr.to_v6_prefix()),
        }
    }
}

impl ToPrefix for Ipv4Addr {
    fn to_prefix(&self) -> IpPrefix {
        IpPrefix::V4(self.to_v4_prefix())
    }
}

impl ToPrefix for Ipv6Addr {
    fn to_prefix(&self) -> IpPrefix {
        IpPrefix::V6(self.to_v6_prefix())
    }
}

impl ToPrefix for IpNet {
    fn to_prefix(&self) -> IpPrefix {
        match self {
            IpNet::V4(ipv4_net) => IpPrefix::V4(ipv4_net.to_v4_prefix()),
            IpNet::V6(ipv6_net) => IpPrefix::V6(ipv6_net.to_v6_prefix()),
        }
    }
}

impl ToV4Prefix for Ipv4Addr {
    fn to_v4_prefix(&self) -> Ipv4Prefix {
        // Ipv4Net::from can construct a /32 for us.
        Ipv4Prefix {
            prefix: Ipv4Net::from(*self),
        }
    }
}

impl ToV4Prefix for Ipv4Net {
    fn to_v4_prefix(&self) -> Ipv4Prefix {
        Ipv4Prefix {
            prefix: self.trunc(),
        }
    }
}

impl ToV6Prefix for Ipv6Addr {
    fn to_v6_prefix(&self) -> Ipv6Prefix {
        // Ipv6Net::from can construct a /128 for us.
        Ipv6Prefix {
            prefix: Ipv6Net::from(*self),
        }
    }
}

impl ToV6Prefix for Ipv6Net {
    fn to_v6_prefix(&self) -> Ipv6Prefix {
        Ipv6Prefix {
            prefix: self.trunc(),
        }
    }
}

// Other stdlib trait implementations

impl Ord for IpPrefix {
    fn cmp(&self, other: &Self) -> Ordering {
        use IpPrefix::*;
        match (self, other) {
            (V4(_), V6(_)) => Ordering::Less,
            (V6(_), V4(_)) => Ordering::Greater,
            (V4(p1), V4(p2)) => p1.cmp(p2),
            (V6(p1), V6(p2)) => p1.cmp(p2),
        }
    }
}

impl PartialOrd for IpPrefix {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Display for IpPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpPrefix::V4(ipv4_prefix) => ipv4_prefix.fmt(f),
            IpPrefix::V6(ipv6_prefix) => ipv6_prefix.fmt(f),
        }
    }
}

impl Display for Ipv4Prefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.prefix.fmt(f)
    }
}
impl Display for Ipv6Prefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.prefix.fmt(f)
    }
}

impl FromStr for IpPrefix {
    type Err = PrefixError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        IpNet::from_str(s)
            .map_err(PrefixError::from)
            .and_then(IpPrefix::try_from)
    }
}

impl FromStr for Ipv4Prefix {
    type Err = PrefixError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ipv4Net::from_str(s)
            .map_err(PrefixError::from)
            .and_then(Ipv4Prefix::try_from)
    }
}

impl FromStr for Ipv6Prefix {
    type Err = PrefixError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ipv6Net::from_str(s)
            .map_err(PrefixError::from)
            .and_then(Ipv6Prefix::try_from)
    }
}

impl TryFrom<IpNet> for IpPrefix {
    type Error = PrefixError;

    fn try_from(value: IpNet) -> Result<Self, Self::Error> {
        match value {
            IpNet::V4(ipv4_net) => Ipv4Prefix::try_from(ipv4_net).map(Self::V4),
            IpNet::V6(ipv6_net) => Ipv6Prefix::try_from(ipv6_net).map(Self::V6),
        }
    }
}

impl TryFrom<Ipv4Net> for Ipv4Prefix {
    type Error = PrefixError;

    fn try_from(value: Ipv4Net) -> Result<Self, Self::Error> {
        let is_canonical_representation = value.addr() == value.network();
        is_canonical_representation
            .then_some(Self { prefix: value })
            .ok_or(PrefixError::NonCanonicalRepresentation)
    }
}

impl TryFrom<Ipv6Net> for Ipv6Prefix {
    type Error = PrefixError;

    fn try_from(value: Ipv6Net) -> Result<Self, Self::Error> {
        let is_canonical_representation = value.addr() == value.network();
        is_canonical_representation
            .then_some(Self { prefix: value })
            .ok_or(PrefixError::NonCanonicalRepresentation)
    }
}

impl TryFrom<(IpAddr, u8)> for IpPrefix {
    type Error = PrefixError;

    fn try_from(value: (IpAddr, u8)) -> Result<Self, Self::Error> {
        let (addr, prefix_length) = value;
        IpNet::new(addr, prefix_length)
            .map_err(PrefixError::from)
            .and_then(Self::try_from)
    }
}

impl From<IpPrefix> for IpNet {
    fn from(value: IpPrefix) -> Self {
        match value {
            IpPrefix::V4(v4) => IpNet::V4(v4.into()),
            IpPrefix::V6(v6) => IpNet::V6(v6.into()),
        }
    }
}

impl From<Ipv4Prefix> for Ipv4Net {
    fn from(value: Ipv4Prefix) -> Self {
        value.prefix
    }
}

impl From<Ipv6Prefix> for Ipv6Net {
    fn from(value: Ipv6Prefix) -> Self {
        value.prefix
    }
}

#[cfg(feature = "ipnetwork")]
impl From<Ipv4Prefix> for ipnetwork::Ipv4Network {
    fn from(value: Ipv4Prefix) -> Self {
        let prefix = value.prefix;
        let addr = prefix.addr();
        let length = prefix.prefix_len();
        // If Ipv4Network::new() doesn't accept what we got out of
        // ipnet::Ipv4Net, something has gone very wrong and we should just
        // panic.
        Self::new(addr, length).expect(
        "Ipv4Network::new() returned an unexpected Err (this shouldn't happen, please file a bug)"
    )
    }
}

#[cfg(feature = "ipnetwork")]
impl From<Ipv6Prefix> for ipnetwork::Ipv6Network {
    fn from(value: Ipv6Prefix) -> Self {
        let prefix = value.prefix;
        let addr = prefix.addr();
        let length = prefix.prefix_len();
        // If Ipv6Network::new() doesn't accept what we got out of
        // ipnet::Ipv6Net, something has gone very wrong and we should just
        // panic.
        Self::new(addr, length).expect(
        "Ipv6Network::new() returned an unexpected Err (this shouldn't happen, please file a bug)"
    )
    }
}

#[cfg(feature = "ipnetwork")]
impl TryFrom<ipnetwork::IpNetwork> for IpPrefix {
    type Error = PrefixError;

    fn try_from(value: ipnetwork::IpNetwork) -> Result<Self, Self::Error> {
        let addr = value.ip();
        let prefix_length = value.prefix();
        IpNet::new(addr, prefix_length)
            .map_err(PrefixError::from)
            .and_then(Self::try_from)
    }
}

//
// Implementations of foreign traits on our types
//

// This implementation is not particularly elegant but sqlx doesn't give
// us the tools we'd to do it properly. Really what we want is the generic
// equivalent of `PgTypeInfo::CIDR`, but even if we wanted to implement
// `sqlx::Type<Postgres>` without being generic over the database, that `CIDR`
// item is private and we can't reference it. So, let's just use the ipnetwork
// implementation as a stepping stone.
#[cfg(feature = "sqlx")]
impl<DB> sqlx::Type<DB> for IpPrefix
where
    DB: sqlx::Database,
    IpNetwork: sqlx::Type<DB>,
{
    fn type_info() -> <DB as sqlx::Database>::TypeInfo {
        ipnetwork::IpNetwork::type_info()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    /// Parse an `IpPrefix` in a test, panicking on failure with a useful label.
    fn prefix(s: &str) -> IpPrefix {
        IpPrefix::from_str(s).unwrap_or_else(|e| panic!("couldn't parse prefix {s:?}: {e}"))
    }

    #[test]
    fn ipv4_prefix_from_str_accepts_and_rejects() {
        scenarios!(
            run = // The Ok payload (the parsed prefix) varies per row, so success is
            // collapsed to the unit value; the contract under test is which
            // inputs parse and which are rejected.
            |s| Ipv4Prefix::from_str(s).map(|_| ()).map_err(drop);
            "canonical /16" {
                "192.168.0.0/16" => Yields(()),
            }

            "canonical /0" {
                "0.0.0.0/0" => Yields(()),
            }

            "host /32" {
                "10.0.0.1/32" => Yields(()),
            }

            "non-canonical host bits set" {
                "192.168.1.2/16" => Fails,
            }

            "non-canonical single low bit" {
                "10.0.0.1/24" => Fails,
            }

            "prefix length out of range" {
                "192.168.0.0/33" => Fails,
            }

            "garbage address" {
                "not-an-ip/16" => Fails,
            }

            "missing prefix length" {
                "192.168.0.0" => Fails,
            }

            "empty string" {
                "" => Fails,
            }

            "ipv6 text rejected by v4 parser" {
                "2001:db8::/48" => Fails,
            }
        );
    }

    #[test]
    fn ipv4_prefix_from_str_round_trips_canonical_inputs() {
        value_scenarios!(
            run = |s| Ipv4Prefix::from_str(s).is_ok();
            "canonical /16 parses" {
                "192.168.0.0/16" => true,
            }

            "non-canonical /16 rejected" {
                "192.168.1.2/16" => false,
            }

            "length 33 rejected" {
                "192.168.0.0/33" => false,
            }

            "garbage rejected" {
                "x/16" => false,
            }
        );
    }

    #[test]
    fn ipv6_prefix_from_str_round_trips_canonical_inputs() {
        value_scenarios!(
            run = |s| Ipv6Prefix::from_str(s).is_ok();
            "canonical /48 parses" {
                "2001:db8::/48" => true,
            }

            "canonical /0 parses" {
                "::/0" => true,
            }

            "host /128 parses" {
                "2001:db8::1/128" => true,
            }

            "non-canonical host bits set" {
                "2001:db8::2/64" => false,
            }

            "length 129 rejected" {
                "2001:db8::/129" => false,
            }

            "garbage rejected" {
                "nope/48" => false,
            }

            "ipv4 text rejected by v6 parser" {
                "10.0.0.0/8" => false,
            }

            "empty string rejected" {
                "" => false,
            }
        );
    }

    #[test]
    fn ip_prefix_from_str_accepts_both_families() {
        value_scenarios!(
            run = |s| IpPrefix::from_str(s).is_ok();
            "ipv4 canonical" {
                "10.0.0.0/8" => true,
            }

            "ipv6 canonical" {
                "2001:db8::/32" => true,
            }

            "ipv4 non-canonical" {
                "10.0.0.1/8" => false,
            }

            "ipv6 non-canonical" {
                "2001:db8::2/64" => false,
            }

            "ipv4 bad length" {
                "10.0.0.0/33" => false,
            }

            "empty" {
                "" => false,
            }
        );
    }

    #[test]
    fn try_from_ipnet_validates_canonical_representation() {
        scenarios!(
            run = |s| {
                let net = IpNet::from_str(s).expect("test net should parse");
                IpPrefix::try_from(net).map(|_| ()).map_err(drop)
            };
            "canonical v4 accepted" {
                "10.0.0.0/8" => Yields(()),
            }

            "non-canonical v4 rejected" {
                "10.0.0.1/8" => Fails,
            }

            "canonical v6 accepted" {
                "2001:db8::/32" => Yields(()),
            }

            "non-canonical v6 rejected" {
                "2001:db8::1/32" => Fails,
            }
        );
    }

    #[test]
    fn try_from_addr_and_length_validates() {
        scenarios!(
            run = |(addr, len)| IpPrefix::try_from((addr, len)).map(|_| ()).map_err(drop);
            "canonical v4" {
                (IpAddr::from([10, 0, 0, 0]), 8u8) => Yields(()),
            }

            "non-canonical v4" {
                (IpAddr::from([10, 0, 0, 1]), 8u8) => Fails,
            }

            "v4 length too long" {
                (IpAddr::from([10, 0, 0, 0]), 33u8) => Fails,
            }

            "v4 host /32" {
                (IpAddr::from([10, 0, 0, 1]), 32u8) => Yields(()),
            }

            "v6 length too long" {
                (IpAddr::from(Ipv6Addr::LOCALHOST), 129u8) => Fails,
            }
        );
    }

    #[test]
    fn address_family_reports_variant() {
        value_scenarios!(
            run = |s| prefix(s).address_family();
            "v4 prefix" {
                "10.0.0.0/8" => IpAddressFamily::Ipv4,
            }

            "v6 prefix" {
                "2001:db8::/32" => IpAddressFamily::Ipv6,
            }
        );
    }

    #[test]
    fn is_address_family_matches_only_its_own() {
        value_scenarios!(
            run = |(s, family)| prefix(s).is_address_family(family);
            "v4 is v4" {
                ("10.0.0.0/8", IpAddressFamily::Ipv4) => true,
            }

            "v4 is not v6" {
                ("10.0.0.0/8", IpAddressFamily::Ipv6) => false,
            }

            "v6 is v6" {
                ("2001:db8::/32", IpAddressFamily::Ipv6) => true,
            }

            "v6 is not v4" {
                ("2001:db8::/32", IpAddressFamily::Ipv4) => false,
            }
        );
    }

    #[test]
    fn require_address_family_or_else_passes_or_runs_fallback() {
        scenarios!(
            run = |(s, family)| {
                prefix(s)
                    .require_address_family_or_else(family, |_| 42)
                    .map(|_| ())
            };
            "v4 required and present" {
                ("10.0.0.0/8", IpAddressFamily::Ipv4) => Yields(()),
            }

            "v6 required but is v4" {
                ("10.0.0.0/8", IpAddressFamily::Ipv6) => FailsWith(42),
            }

            "v6 required and present" {
                ("2001:db8::/32", IpAddressFamily::Ipv6) => Yields(()),
            }

            "v4 required but is v6" {
                ("2001:db8::/32", IpAddressFamily::Ipv4) => FailsWith(42),
            }
        );
    }

    #[test]
    fn contains_checks_membership_across_families() {
        value_scenarios!(
            run = |(p, addr)| prefix(p).contains(addr);
            "v4 prefix holds member address" {
                ("10.0.0.0/8", IpAddr::from([10, 0, 0, 1])) => true,
            }

            "v4 prefix holds its own network address" {
                ("10.0.0.0/8", IpAddr::from([10, 0, 0, 0])) => true,
            }

            "v4 prefix excludes outside address" {
                ("10.0.0.0/8", IpAddr::from([11, 0, 0, 1])) => false,
            }

            "v4 prefix does not hold a v6 address" {
                ("10.0.0.0/8", IpAddr::from(Ipv6Addr::LOCALHOST)) => false,
            }

            "default route holds anything v4" {
                ("0.0.0.0/0", IpAddr::from([200, 1, 2, 3])) => true,
            }

            "v6 prefix holds member address" {
                ("2001:db8::/32", IpAddr::from_str("2001:db8::1").unwrap()) => true,
            }

            "v6 prefix excludes outside address" {
                ("2001:db8::/32", IpAddr::from_str("2001:db9::1").unwrap()) => false,
            }

            "v6 prefix does not hold a v4 address" {
                ("2001:db8::/32", IpAddr::from([10, 0, 0, 1])) => false,
            }
        );
    }

    #[test]
    fn contains_subprefix_relationships() {
        value_scenarios!(
            run = |(outer, inner)| prefix(outer).contains(prefix(inner));
            "broader holds narrower" {
                ("10.0.0.0/8", "10.1.0.0/16") => true,
            }

            "prefix holds itself" {
                ("10.0.0.0/8", "10.0.0.0/8") => true,
            }

            "narrower does not hold broader" {
                ("10.0.0.0/16", "10.0.0.0/8") => false,
            }

            "disjoint v4 prefixes" {
                ("10.0.0.0/8", "11.0.0.0/8") => false,
            }

            "v4 never holds v6" {
                ("10.0.0.0/8", "2001:db8::/32") => false,
            }

            "v6 broader holds narrower" {
                ("2001:db8::/32", "2001:db8:1::/48") => true,
            }
        );
    }

    #[test]
    fn prefix_length_reports_mask_bits() {
        value_scenarios!(
            run = |s| prefix(s).prefix_length();
            "v4 /8" {
                "10.0.0.0/8" => 8usize,
            }

            "v4 /0" {
                "0.0.0.0/0" => 0usize,
            }

            "v4 /32" {
                "10.0.0.1/32" => 32usize,
            }

            "v6 /48" {
                "2001:db8::/48" => 48usize,
            }

            "v6 /128" {
                "2001:db8::1/128" => 128usize,
            }
        );
    }

    #[test]
    fn ordering_sorts_by_family_then_prefix() {
        value_scenarios!(
            run = |(a, b)| prefix(a).cmp(&prefix(b));
            "shorter prefix sorts before longer at same address" {
                ("10.0.0.0/8", "10.0.0.0/16") => Ordering::Less,
            }

            "longer prefix sorts after shorter" {
                ("10.0.0.0/16", "10.0.0.0/8") => Ordering::Greater,
            }

            "equal prefixes compare equal" {
                ("10.0.0.0/8", "10.0.0.0/8") => Ordering::Equal,
            }

            "lower address sorts first" {
                ("10.0.0.0/8", "11.0.0.0/8") => Ordering::Less,
            }

            "v4 always sorts before v6" {
                ("10.0.0.0/16", "2001:db8::/32") => Ordering::Less,
            }

            "v6 always sorts after v4" {
                ("2001:db8::/32", "10.0.0.0/16") => Ordering::Greater,
            }

            "two v6 by address" {
                ("2001:db8::/32", "2001:db9::/32") => Ordering::Less,
            }
        );
    }

    #[test]
    fn display_renders_canonical_text() {
        value_scenarios!(
            run = |s| prefix(s).to_string();
            "v4 prefix" {
                "10.0.0.0/8" => "10.0.0.0/8".to_string(),
            }

            "v4 default route" {
                "0.0.0.0/0" => "0.0.0.0/0".to_string(),
            }

            "v6 prefix lowercased" {
                "2001:DB8::/32" => "2001:db8::/32".to_string(),
            }
        );
    }

    #[test]
    fn bifurcate_splits_into_two_halves() {
        value_scenarios!(
            run = |s| {
                prefix(s)
                    .bifurcate()
                    .map(|(even, odd)| (even.to_string(), odd.to_string()))
            };
            "v4 /24 splits at the midpoint" {
                "10.0.0.0/24" => Some(("10.0.0.0/25".to_string(), "10.0.0.128/25".to_string())),
            }

            "v4 /0 splits the whole space" {
                "0.0.0.0/0" => Some(("0.0.0.0/1".to_string(), "128.0.0.0/1".to_string())),
            }

            "v4 /32 cannot split" {
                "10.0.0.1/32" => None,
            }

            "v6 /32 splits at the midpoint" {
                "2001:db8::/32" => Some((
                    "2001:db8::/33".to_string(),
                    "2001:db8:8000::/33".to_string(),
                )),
            }

            "v6 /128 cannot split" {
                "2001:db8::1/128" => None,
            }
        );
    }

    #[test]
    fn get_sibling_flips_the_last_prefix_bit() {
        value_scenarios!(
            run = |s| prefix(s).get_sibling().map(|p| p.to_string());
            "v4 /24 even sibling" {
                "10.0.0.0/24" => Some("10.0.1.0/24".to_string()),
            }

            "v4 /24 odd sibling flips back" {
                "10.0.1.0/24" => Some("10.0.0.0/24".to_string()),
            }

            "v4 /1 sibling" {
                "0.0.0.0/1" => Some("128.0.0.0/1".to_string()),
            }

            "v4 /0 has no sibling" {
                "0.0.0.0/0" => None,
            }

            "v6 /34 even sibling" {
                "2001:db8::/34" => Some("2001:db8:4000::/34".to_string()),
            }

            "v6 /34 odd sibling flips back" {
                "2001:db8:4000::/34" => Some("2001:db8::/34".to_string()),
            }

            "v6 /0 has no sibling" {
                "::/0" => None,
            }
        );
    }

    #[test]
    fn get_last_subprefix_is_the_all_ones_host() {
        value_scenarios!(
            run = |s| prefix(s).get_last_subprefix().to_string();
            "v4 /24 last host" {
                "10.0.0.0/24" => "10.0.0.255/32".to_string(),
            }

            "v4 /8 last host" {
                "10.0.0.0/8" => "10.255.255.255/32".to_string(),
            }

            "v4 /32 is its own last host" {
                "10.0.0.1/32" => "10.0.0.1/32".to_string(),
            }

            "v6 /32 last host" {
                "2001:db8::/32" => "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff/128".to_string(),
            }
        );
    }

    #[test]
    fn try_aggregate_combines_or_declines() {
        value_scenarios!(
            run = |(a, b)| prefix(a).try_aggregate(&prefix(b)).map(|p| p.to_string());
            "siblings aggregate to their supernet" {
                ("10.0.0.0/25", "10.0.0.128/25") => Some("10.0.0.0/24".to_string()),
            }

            "containing prefix absorbs the contained" {
                ("10.0.0.0/8", "10.1.0.0/16") => Some("10.0.0.0/8".to_string()),
            }

            "contained side absorbed by container" {
                ("10.1.0.0/16", "10.0.0.0/8") => Some("10.0.0.0/8".to_string()),
            }

            "non-adjacent prefixes do not aggregate" {
                ("10.0.0.0/24", "10.0.2.0/24") => None,
            }

            "sibling /24s aggregate to their /23 supernet" {
                ("10.0.0.0/24", "10.0.1.0/24") => Some("10.0.0.0/23".to_string()),
            }

            "different families do not aggregate" {
                ("10.0.0.0/8", "2001:db8::/32") => None,
            }

            "v6 siblings aggregate" {
                ("2001:db8::/33", "2001:db8:8000::/33") => Some("2001:db8::/32".to_string()),
            }
        );
    }

    #[test]
    fn to_prefix_from_addresses_and_nets() {
        value_scenarios!(
            run = |s| IpAddr::from_str(s).unwrap().to_prefix().to_string();
            "ipv4 address becomes /32" {
                "10.0.0.1" => "10.0.0.1/32".to_string(),
            }

            "ipv6 address becomes /128" {
                "2001:db8::1" => "2001:db8::1/128".to_string(),
            }
        );
    }

    #[test]
    fn to_prefix_from_ipnet_truncates_host_bits() {
        value_scenarios!(
            run = |s| IpNet::from_str(s).unwrap().to_prefix().to_string();
            "v4 net truncated to network address" {
                "10.0.0.1/8" => "10.0.0.0/8".to_string(),
            }

            "v4 already canonical" {
                "10.0.0.0/8" => "10.0.0.0/8".to_string(),
            }

            "v6 net truncated to network address" {
                "2001:db8::1/32" => "2001:db8::/32".to_string(),
            }
        );
    }

    #[test]
    fn into_inner_round_trips_through_ipnet() {
        value_scenarios!(
            run = |s| {
                let net = IpNet::from_str(s).unwrap();
                let prefix = IpPrefix::try_from(net).unwrap();
                IpNet::from(prefix) == net
            };
            "v4 round-trips" {
                "10.0.0.0/8" => true,
            }

            "v6 round-trips" {
                "2001:db8::/32" => true,
            }
        );
    }
}
