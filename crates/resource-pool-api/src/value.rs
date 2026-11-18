// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Checks provider values and allocation constraints without expanding ranges.

use std::net::{Ipv4Addr, Ipv6Addr};

use tonic::Status;

use crate::v1::address_prefix::Prefix;
use crate::v1::eligibility::Entries;
use crate::v1::value::Kind;
use crate::v1::{
    AssignmentMode, Eligibility, Ipv4Prefix, Ipv6Prefix, Value, ValueDomain, ValueRange,
};

impl Value {
    /// `validate` checks the value's encoding and expected pool type. Missing
    /// variants, wrong address lengths, and prefixes with host bits return
    /// `InvalidArgument`; integer values accept the full signed 64-bit range.
    pub fn validate(&self, domain: ValueDomain) -> Result<(), Status> {
        if self.domain()? != domain {
            return Err(Status::invalid_argument(
                "value does not match the pool domain",
            ));
        }
        Ok(())
    }

    /// `domain` checks the wire encoding before identifying the value type.
    fn domain(&self) -> Result<ValueDomain, Status> {
        match self.kind.as_ref() {
            Some(Kind::Integer(_)) => Ok(ValueDomain::Integer),
            Some(Kind::Ipv4(address)) => {
                address_bytes::<4>(address)?;
                Ok(ValueDomain::IpAddress)
            }
            Some(Kind::Ipv6(address)) => {
                address_bytes::<16>(address)?;
                Ok(ValueDomain::IpAddress)
            }
            Some(Kind::Ipv6Prefix(prefix)) => {
                prefix.network_number()?;
                Ok(ValueDomain::Ipv6Prefix)
            }
            None => Err(Status::invalid_argument("value kind is required")),
        }
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self {
            kind: Some(Kind::Integer(value)),
        }
    }
}

impl From<Ipv4Addr> for Value {
    fn from(address: Ipv4Addr) -> Self {
        Self {
            kind: Some(Kind::Ipv4(address.octets().to_vec())),
        }
    }
}

impl From<Ipv6Addr> for Value {
    fn from(address: Ipv6Addr) -> Self {
        Self {
            kind: Some(Kind::Ipv6(address.octets().to_vec())),
        }
    }
}

impl Eligibility {
    /// `validate` checks one complete definition entry before a provider uses
    /// it. Empty integer and address ranges are valid; malformed entries return
    /// `InvalidArgument` even when the provider has no values in that entry.
    pub fn validate(&self, domain: ValueDomain) -> Result<(), Status> {
        match AssignmentMode::try_from(self.assignment_mode) {
            Ok(AssignmentMode::Automatic | AssignmentMode::Manual) => {}
            _ => {
                return Err(Status::invalid_argument(
                    "assignment mode must be automatic or manual",
                ));
            }
        }
        match self.entries.as_ref() {
            Some(Entries::Value(value)) => value.validate(domain),
            Some(Entries::Range(range)) => range.validate(domain),
            Some(Entries::AddressPrefix(prefix)) if domain == ValueDomain::IpAddress => {
                match prefix.prefix.as_ref() {
                    Some(Prefix::Ipv4(prefix)) => prefix.network_number().map(drop),
                    Some(Prefix::Ipv6(prefix)) => prefix.network_number().map(drop),
                    None => Err(Status::invalid_argument("address prefix is required")),
                }
            }
            Some(Entries::DelegatedPrefix(delegation)) if domain == ValueDomain::Ipv6Prefix => {
                let prefix = delegation
                    .prefix
                    .as_ref()
                    .ok_or_else(|| Status::invalid_argument("delegation prefix is required"))?;
                prefix.network_number()?;
                if delegation.delegate_prefix_len <= prefix.prefix_len
                    || delegation.delegate_prefix_len > 128
                {
                    return Err(Status::invalid_argument(
                        "delegated prefix length must be greater than the parent length and at most 128",
                    ));
                }
                Ok(())
            }
            Some(_) => Err(Status::invalid_argument(
                "definition entry does not match the pool domain",
            )),
            None => Err(Status::invalid_argument("definition entry is required")),
        }
    }

    /// `contains` checks a candidate against this entry without listing its
    /// values. It validates both inputs and leaves assignment-mode selection to
    /// the caller. Ranges include their start and exclude their end; IPv4
    /// prefixes include the network address and exclude the broadcast address.
    /// Malformed inputs or different value domains return `InvalidArgument`.
    /// IPv4 and IPv6 share a domain, but never match each other's entries.
    pub fn contains(&self, value: &Value) -> Result<bool, Status> {
        self.validate(value.domain()?)?;
        match (self.entries.as_ref(), value.kind.as_ref()) {
            (Some(Entries::Value(expected)), _) => Ok(expected == value),
            (Some(Entries::Range(range)), Some(candidate)) => Ok(range.contains(candidate)),
            (Some(Entries::AddressPrefix(prefix)), Some(Kind::Ipv4(address))) => {
                let Some(Prefix::Ipv4(prefix)) = prefix.prefix.as_ref() else {
                    return Ok(false);
                };
                let address = u32::from_be_bytes(address_bytes(address)?);
                let network = prefix.network_number()?;
                let mask = u32::MAX.checked_shl(32 - prefix.prefix_len).unwrap_or(0);
                Ok(address & mask == network && address != (network | !mask))
            }
            (Some(Entries::AddressPrefix(prefix)), Some(Kind::Ipv6(address))) => {
                let Some(Prefix::Ipv6(prefix)) = prefix.prefix.as_ref() else {
                    return Ok(false);
                };
                Ok(
                    u128::from_be_bytes(address_bytes(address)?) & prefix_mask(prefix.prefix_len)
                        == prefix.network_number()?,
                )
            }
            (Some(Entries::DelegatedPrefix(delegation)), Some(Kind::Ipv6Prefix(candidate))) => {
                let prefix = delegation
                    .prefix
                    .as_ref()
                    .ok_or_else(|| Status::invalid_argument("delegation prefix is required"))?;
                Ok(candidate.prefix_len == delegation.delegate_prefix_len
                    && candidate.network_number()? & prefix_mask(prefix.prefix_len)
                        == prefix.network_number()?)
            }
            _ => Ok(false),
        }
    }
}

impl ValueRange {
    /// `validate` requires matching endpoint types and prefix lengths. Prefix
    /// ranges must advance by at least one whole prefix.
    fn validate(&self, domain: ValueDomain) -> Result<(), Status> {
        let start = self
            .start
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("range start is required"))?;
        let end = self
            .end
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("range end is required"))?;
        start.validate(domain)?;
        end.validate(domain)?;
        match (start.kind.as_ref(), end.kind.as_ref()) {
            (Some(Kind::Integer(_)), Some(Kind::Integer(_)))
            | (Some(Kind::Ipv4(_)), Some(Kind::Ipv4(_)))
            | (Some(Kind::Ipv6(_)), Some(Kind::Ipv6(_))) => Ok(()),
            (Some(Kind::Ipv6Prefix(start)), Some(Kind::Ipv6Prefix(end)))
                if start.prefix_len == end.prefix_len && start.network < end.network =>
            {
                Ok(())
            }
            _ => Err(Status::invalid_argument(
                "range endpoints must have matching types; prefix endpoints also require equal lengths and start before end",
            )),
        }
    }

    /// `contains` compares already validated endpoints in network byte order.
    fn contains(&self, candidate: &Kind) -> bool {
        let endpoints = self
            .start
            .as_ref()
            .and_then(|value| value.kind.as_ref())
            .zip(self.end.as_ref().and_then(|value| value.kind.as_ref()));
        match (endpoints, candidate) {
            (Some((Kind::Integer(start), Kind::Integer(end))), Kind::Integer(value)) => {
                start <= value && value < end
            }
            (Some((Kind::Ipv4(start), Kind::Ipv4(end))), Kind::Ipv4(value))
            | (Some((Kind::Ipv6(start), Kind::Ipv6(end))), Kind::Ipv6(value)) => {
                start <= value && value < end
            }
            (Some((Kind::Ipv6Prefix(start), Kind::Ipv6Prefix(end))), Kind::Ipv6Prefix(value)) => {
                value.prefix_len == start.prefix_len
                    && start.network <= value.network
                    && value.network < end.network
            }
            _ => false,
        }
    }
}

impl Ipv4Prefix {
    /// `network_number` checks the prefix length and rejects addresses with
    /// host bits, so equivalent prefixes have the same wire representation.
    fn network_number(&self) -> Result<u32, Status> {
        if self.prefix_len > 32 {
            return Err(Status::invalid_argument(
                "IPv4 prefix length must be at most 32",
            ));
        }
        let network = u32::from_be_bytes(address_bytes(&self.network)?);
        let mask = u32::MAX.checked_shl(32 - self.prefix_len).unwrap_or(0);
        if network & mask != network {
            return Err(Status::invalid_argument(
                "IPv4 prefix network has host bits set",
            ));
        }
        Ok(network)
    }
}

impl Ipv6Prefix {
    /// `network_number` checks the prefix length and rejects addresses with
    /// host bits, so equivalent prefixes have the same wire representation.
    fn network_number(&self) -> Result<u128, Status> {
        if self.prefix_len > 128 {
            return Err(Status::invalid_argument(
                "IPv6 prefix length must be at most 128",
            ));
        }
        let network = u128::from_be_bytes(address_bytes(&self.network)?);
        if network & prefix_mask(self.prefix_len) != network {
            return Err(Status::invalid_argument(
                "IPv6 prefix network has host bits set",
            ));
        }
        Ok(network)
    }
}

/// `address_bytes` enforces the fixed address length before numeric conversion.
fn address_bytes<const N: usize>(address: &[u8]) -> Result<[u8; N], Status> {
    address
        .try_into()
        .map_err(|_| Status::invalid_argument(format!("address must contain {N} bytes")))
}

/// `prefix_mask` also handles `/0`, whose mask contains no network bits.
fn prefix_mask(prefix_len: u32) -> u128 {
    u128::MAX.checked_shl(128 - prefix_len).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios};
    use prost::Message;
    use tonic::Code;

    use super::*;
    use crate::v1::{AddressPrefix, DelegatedPrefix};

    /// `prefix` keeps IPv6 fixture addresses readable at the call site.
    fn prefix(network: &str, prefix_len: u32) -> Ipv6Prefix {
        Ipv6Prefix {
            network: network.parse::<Ipv6Addr>().unwrap().octets().to_vec(),
            prefix_len,
        }
    }

    /// `prefix_value` wraps a whole IPv6 subnet as one allocatable value.
    fn prefix_value(network: &str, prefix_len: u32) -> Value {
        Value {
            kind: Some(Kind::Ipv6Prefix(prefix(network, prefix_len))),
        }
    }

    /// `entry` supplies the ordinary automatic partition for constraint tests.
    fn entry(entries: Entries) -> Eligibility {
        Eligibility {
            assignment_mode: AssignmentMode::Automatic as i32,
            entries: Some(entries),
        }
    }

    /// `range` makes the inclusive start and exclusive end visible in cases.
    fn range(start: Value, end: Value) -> Entries {
        Entries::Range(ValueRange {
            start: Some(start),
            end: Some(end),
        })
    }

    #[test]
    fn values_keep_their_type_and_bytes_across_protobuf() {
        let integers = [
            ("minimum signed integer", i64::MIN),
            ("maximum signed integer", i64::MAX),
        ]
        .map(|(scenario, integer)| Case {
            scenario,
            input: (ValueDomain::Integer, Value::from(integer)),
            expect: Yields(Value {
                kind: Some(Kind::Integer(integer)),
            }),
        });
        let addresses_and_prefixes = [
            (
                "IPv4 address",
                ValueDomain::IpAddress,
                Ipv4Addr::new(192, 0, 2, 1).into(),
            ),
            (
                "IPv6 address",
                ValueDomain::IpAddress,
                Ipv6Addr::from(u128::MAX).into(),
            ),
            (
                "IPv6 default route",
                ValueDomain::Ipv6Prefix,
                prefix_value("::", 0),
            ),
            (
                "IPv6 host prefix",
                ValueDomain::Ipv6Prefix,
                prefix_value("2001:db8::1", 128),
            ),
        ]
        .into_iter()
        .map(|(scenario, domain, value)| Case {
            scenario,
            expect: Yields(value.clone()),
            input: (domain, value),
        });
        check_cases(
            integers.into_iter().chain(addresses_and_prefixes),
            |(domain, value)| {
                let decoded = Value::decode(value.encode_to_vec().as_slice()).unwrap();
                decoded.validate(domain).map_err(|error| error.code())?;
                Ok::<_, Code>(decoded)
            },
        );
    }

    #[test]
    fn invalid_wire_values_do_not_enter_a_pool() {
        scenarios!(run = |(domain, value): (ValueDomain, Value)| value.validate(domain).map_err(|error| error.code());
            "required type and domain" {
                (ValueDomain::Integer, Value { kind: None }) => FailsWith(Code::InvalidArgument),
                (ValueDomain::Unspecified, 1.into()) => FailsWith(Code::InvalidArgument),
                (ValueDomain::Integer, Ipv4Addr::LOCALHOST.into()) => FailsWith(Code::InvalidArgument),
            }
            "address byte lengths" {
                (ValueDomain::IpAddress, Value { kind: Some(Kind::Ipv4(vec![0; 16])) }) => FailsWith(Code::InvalidArgument),
                (ValueDomain::IpAddress, Value { kind: Some(Kind::Ipv6(vec![0; 4])) }) => FailsWith(Code::InvalidArgument),
            }
            "IPv6 subnet encoding" {
                (ValueDomain::Ipv6Prefix, prefix_value("2001:db8::1", 64)) => FailsWith(Code::InvalidArgument),
                (ValueDomain::Ipv6Prefix, prefix_value("::", 129)) => FailsWith(Code::InvalidArgument),
            }
        );
    }

    #[test]
    fn eligibility_checks_boundaries_without_expanding_ranges() {
        scenarios!(run = |(entries, value)| entry(entries).contains(&value).map_err(|error| error.code());
            "singleton requires the same value" {
                (Entries::Value(1.into()), 1.into()) => Yields(true),
                (Entries::Value(1.into()), 2.into()) => Yields(false),
            }
            "signed range includes start and excludes end" {
                (range(i64::MIN.into(), i64::MAX.into()), i64::MIN.into()) => Yields(true),
                (range(i64::MIN.into(), i64::MAX.into()), i64::MAX.into()) => Yields(false),
                (range(2.into(), 1.into()), 0.into()) => Yields(false),
            }
            "address ranges compare complete addresses and exclude end" {
                (range(Ipv4Addr::new(192, 0, 2, 255).into(), Ipv4Addr::new(192, 0, 3, 1).into()), Ipv4Addr::new(192, 0, 2, 255).into()) => Yields(true),
                (range(Ipv4Addr::new(192, 0, 2, 255).into(), Ipv4Addr::new(192, 0, 3, 1).into()), Ipv4Addr::new(192, 0, 3, 0).into()) => Yields(true),
                (range(Ipv4Addr::new(192, 0, 2, 255).into(), Ipv4Addr::new(192, 0, 3, 1).into()), Ipv4Addr::new(192, 0, 3, 1).into()) => Yields(false),
                (range(Ipv6Addr::from(255u128).into(), Ipv6Addr::from(257u128).into()), Ipv6Addr::from(254u128).into()) => Yields(false),
                (range(Ipv6Addr::from(255u128).into(), Ipv6Addr::from(257u128).into()), Ipv6Addr::from(256u128).into()) => Yields(true),
                (range(Ipv6Addr::from(255u128).into(), Ipv6Addr::from(257u128).into()), Ipv6Addr::from(257u128).into()) => Yields(false),
            }
            "IPv4 prefix includes network and excludes broadcast" {
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![192, 0, 2, 0], prefix_len: 24 })) }), Ipv4Addr::new(192, 0, 2, 0).into()) => Yields(true),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![192, 0, 2, 0], prefix_len: 24 })) }), Ipv4Addr::new(192, 0, 2, 255).into()) => Yields(false),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![192, 0, 2, 0], prefix_len: 24 })) }), Ipv4Addr::new(192, 0, 3, 1).into()) => Yields(false),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![192, 0, 2, 0], prefix_len: 31 })) }), Ipv4Addr::new(192, 0, 2, 0).into()) => Yields(true),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![192, 0, 2, 0], prefix_len: 31 })) }), Ipv4Addr::new(192, 0, 2, 1).into()) => Yields(false),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![0; 4], prefix_len: 0 })) }), Ipv4Addr::new(192, 0, 2, 1).into()) => Yields(true),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![192, 0, 2, 1], prefix_len: 32 })) }), Ipv4Addr::new(192, 0, 2, 1).into()) => Yields(false),
            }
            "IPv6 prefix includes all addresses" {
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv6(prefix("::", 0))) }), Ipv6Addr::from(u128::MAX).into()) => Yields(true),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv6(prefix("::", 0))) }), Ipv4Addr::LOCALHOST.into()) => Yields(false),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv6(prefix("2001:db8::", 64))) }), "2001:db8::".parse::<Ipv6Addr>().unwrap().into()) => Yields(true),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv6(prefix("2001:db8::", 64))) }), "2001:db8::ffff:ffff:ffff:ffff".parse::<Ipv6Addr>().unwrap().into()) => Yields(true),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv6(prefix("2001:db8::", 64))) }), "2001:db8:0:1::".parse::<Ipv6Addr>().unwrap().into()) => Yields(false),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv6(prefix("2001:db8::1", 128))) }), "2001:db8::1".parse::<Ipv6Addr>().unwrap().into()) => Yields(true),
                (Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv6(prefix("2001:db8::1", 128))) }), "2001:db8::2".parse::<Ipv6Addr>().unwrap().into()) => Yields(false),
            }
            "subnets must match delegated length and parent" {
                (Entries::DelegatedPrefix(DelegatedPrefix { prefix: Some(prefix("2001:db8::", 32)), delegate_prefix_len: 64 }), prefix_value("2001:db8:ffff:ffff::", 64)) => Yields(true),
                (Entries::DelegatedPrefix(DelegatedPrefix { prefix: Some(prefix("2001:db8::", 32)), delegate_prefix_len: 64 }), prefix_value("2001:db9::", 64)) => Yields(false),
                (Entries::DelegatedPrefix(DelegatedPrefix { prefix: Some(prefix("2001:db8::", 32)), delegate_prefix_len: 64 }), prefix_value("2001:db8::", 48)) => Yields(false),
                (Entries::DelegatedPrefix(DelegatedPrefix { prefix: Some(prefix("::", 0)), delegate_prefix_len: 128 }), prefix_value("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff", 128)) => Yields(true),
            }
            "subnet ranges include start and exclude end" {
                (range(prefix_value("2001:db8::", 64), prefix_value("2001:db8:0:2::", 64)), prefix_value("2001:db8::", 64)) => Yields(true),
                (range(prefix_value("2001:db8::", 64), prefix_value("2001:db8:0:2::", 64)), prefix_value("2001:db8:0:1::", 64)) => Yields(true),
                (range(prefix_value("2001:db8::", 64), prefix_value("2001:db8:0:2::", 64)), prefix_value("2001:db8:0:2::", 64)) => Yields(false),
                (range(prefix_value("2001:db8::", 64), prefix_value("2001:db8:0:2::", 64)), prefix_value("2001:db7:ffff:ffff::", 64)) => Yields(false),
                (range(prefix_value("2001:db8::", 64), prefix_value("2001:db8:0:2::", 64)), prefix_value("2001:db8::", 80)) => Yields(false),
            }
        );
    }

    #[test]
    fn eligibility_validation_rejects_malformed_entries() {
        scenarios!(run = |(domain, entries)| entry(entries).validate(domain).map_err(|error| error.code());
            "entry types must match the pool domain" {
                (ValueDomain::Integer, Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![192, 0, 2, 0], prefix_len: 24 })) })) => FailsWith(Code::InvalidArgument),
                (ValueDomain::IpAddress, Entries::DelegatedPrefix(DelegatedPrefix { prefix: Some(prefix("2001:db8::", 32)), delegate_prefix_len: 64 })) => FailsWith(Code::InvalidArgument),
            }
            "range endpoints are required and use matching types" {
                (ValueDomain::Integer, Entries::Range(ValueRange { start: None, end: Some(1.into()) })) => FailsWith(Code::InvalidArgument),
                (ValueDomain::IpAddress, range(Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into())) => FailsWith(Code::InvalidArgument),
            }
            "IPv6 subnet ranges advance by whole equal-length prefixes" {
                (ValueDomain::Ipv6Prefix, range(prefix_value("::", 64), prefix_value("::", 64))) => FailsWith(Code::InvalidArgument),
                (ValueDomain::Ipv6Prefix, range(prefix_value("2001:db8:0:2::", 64), prefix_value("2001:db8::", 64))) => FailsWith(Code::InvalidArgument),
                (ValueDomain::Ipv6Prefix, range(prefix_value("2001:db8::", 64), prefix_value("2001:db8:1::", 48))) => FailsWith(Code::InvalidArgument),
            }
            "prefixes reject host bits and invalid lengths" {
                (ValueDomain::IpAddress, Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![192, 0, 2, 1], prefix_len: 24 })) })) => FailsWith(Code::InvalidArgument),
                (ValueDomain::IpAddress, Entries::AddressPrefix(AddressPrefix { prefix: Some(Prefix::Ipv4(Ipv4Prefix { network: vec![0; 4], prefix_len: 33 })) })) => FailsWith(Code::InvalidArgument),
                (ValueDomain::Ipv6Prefix, Entries::DelegatedPrefix(DelegatedPrefix { prefix: Some(prefix("2001:db8::", 64)), delegate_prefix_len: 64 })) => FailsWith(Code::InvalidArgument),
                (ValueDomain::Ipv6Prefix, Entries::DelegatedPrefix(DelegatedPrefix { prefix: Some(prefix("::", 0)), delegate_prefix_len: 129 })) => FailsWith(Code::InvalidArgument),
            }
        );
        scenarios!(run = |(assignment_mode, entries)| Eligibility { assignment_mode, entries }.validate(ValueDomain::Integer).map_err(|error| error.code());
            "only explicit assignment modes are accepted" {
                (AssignmentMode::Unspecified as i32, Some(Entries::Value(1.into()))) => FailsWith(Code::InvalidArgument),
                (-1, Some(Entries::Value(1.into()))) => FailsWith(Code::InvalidArgument),
                (AssignmentMode::Manual as i32, Some(Entries::Value(1.into()))) => Yields(()),
            }
            "an eligibility entry is required" {
                (AssignmentMode::Automatic as i32, None) => FailsWith(Code::InvalidArgument),
            }
        );
    }
}
