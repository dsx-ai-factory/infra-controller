// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Convert Core definitions and typed values without expanding pool inventory.

use std::any::type_name;
use std::array::TryFromSliceError;
use std::error::Error;
use std::fmt;
use std::net::{AddrParseError, IpAddr, Ipv4Addr, Ipv6Addr};
use std::num::ParseIntError;
use std::str::FromStr;

use carbide_resource_pool_api::v1;
use ipnetwork::{IpNetworkError, Ipv4Network, Ipv6Network};
use model::resource_pool::{
    ResourcePoolDef, ResourcePoolType, ResourcePoolValueDomain, ValueType,
    parse_integer_range_bound,
};
use v1::address_prefix::Prefix;
use v1::eligibility::Entries;

#[cfg(test)]
mod tests;

/// `ValueConversionError` identifies invalid definitions, wire values, and
/// values the Rust consumer cannot represent. Its own `Display` and `Debug`
/// omit input values. `Error::source` preserves parser diagnostics, which may
/// include input text; callers choose whether to inspect or record that source.
#[derive(thiserror::Error)]
pub enum ValueConversionError {
    /// A declaration violates the Core definition contract.
    #[error("invalid resource pool definition: {0}")]
    Definition(&'static str),
    /// An integer could not be parsed as a signed 64-bit value.
    #[error("invalid resource pool integer")]
    Integer(#[from] ParseIntError),
    /// An address could not be parsed in the declared family.
    #[error("invalid resource pool address")]
    Address(#[from] AddrParseError),
    /// A network could not be parsed in the declared family.
    #[error("invalid resource pool network")]
    Network(#[from] IpNetworkError),
    /// A wire value, eligibility declaration, or typed encoding precondition
    /// failed validation.
    #[error("invalid resource pool wire value or eligibility")]
    Wire(#[from] Box<tonic::Status>),
    /// An address does not contain the required number of octets.
    #[error("invalid resource pool address length")]
    AddressLength(#[from] TryFromSliceError),
    /// An allocation does not satisfy the supplied eligibility declarations.
    #[error("invalid resource pool allocation: {0}")]
    Allocation(&'static str),
    /// A valid wire value does not fit the requested Rust type.
    #[error("resource pool value cannot be represented by {rust_type}")]
    Consumer {
        /// Diagnostic type name from `std::any::type_name`; its spelling is not stable.
        rust_type: &'static str,
        /// The consumer's parse error or the error from encoding it back to wire form.
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    /// The consumer parsed and encoded successfully but produced a different wire value.
    #[error("resource pool value changed when converted to {rust_type}")]
    ConsumerValueChanged {
        /// Diagnostic type name from `std::any::type_name`; its spelling is not stable.
        rust_type: &'static str,
    },
}

impl fmt::Debug for ValueConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Network parsers can retain the original input in their errors.
        // Keep it out of routine diagnostics while preserving the source.
        fmt::Display::fmt(self, formatter)
    }
}

/// `ResourcePoolEligibility` holds compact declarations in their original
/// order. The first matching declaration determines a value's assignment mode.
/// Conversion requires exactly one prefix or a nonempty list of ranges and
/// preserves the existing integer and network parsing rules. It does not
/// enumerate values or apply the Integrated database's inventory-size limit.
/// Converting a definition does not reconstruct values added by later growth;
/// an existing pool requires its effective eligibility and saved value domain.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourcePoolEligibility {
    /// Value domain derived from the definition's declared type. For an existing
    /// pool, the caller must compare it with the saved binding's domain.
    pub domain: ResourcePoolValueDomain,
    /// Ordered wire declarations; later overlaps keep the earlier mode.
    pub entries: Vec<v1::Eligibility>,
}

impl TryFrom<ResourcePoolDef> for ResourcePoolEligibility {
    type Error = ValueConversionError;

    fn try_from(definition: ResourcePoolDef) -> Result<Self, Self::Error> {
        let domain = ValueType::from(definition.pool_type).into();
        let entries = match (&definition.prefix, definition.ranges.is_empty()) {
            (None, true) | (Some(_), false) => {
                return Err(ValueConversionError::Definition(
                    "provide exactly one prefix or nonempty ranges",
                ));
            }
            (Some(prefix), true) => vec![v1::Eligibility {
                assignment_mode: v1::AssignmentMode::Automatic as i32,
                entries: Some(prefix_entry(
                    definition.pool_type,
                    prefix,
                    definition.delegate_prefix_len,
                )?),
            }],
            (None, false) => definition
                .ranges
                .iter()
                .map(|range| {
                    Ok(v1::Eligibility {
                        assignment_mode: if range.auto_assign {
                            v1::AssignmentMode::Automatic
                        } else {
                            v1::AssignmentMode::Manual
                        } as i32,
                        entries: Some(Entries::Range(v1::ValueRange {
                            start: Some(range_value(definition.pool_type, &range.start)?),
                            end: Some(range_value(definition.pool_type, &range.end)?),
                        })),
                    })
                })
                .collect::<Result<_, ValueConversionError>>()?,
        };
        for entry in &entries {
            entry.validate(wire_domain(domain)).map_err(Box::new)?;
        }
        Ok(Self { domain, entries })
    }
}

impl ResourcePoolEligibility {
    /// `validate_allocation` checks the authoritative domain, membership, and
    /// first matching declaration's assignment mode.
    /// The caller must supply the accepted eligibility for the original
    /// operation and compare its domain with the saved binding. The caller must
    /// also check revision and owner identity, any requested value, and selection
    /// rules against the saved operation.
    pub fn validate_allocation(
        &self,
        allocation: &v1::Allocation,
    ) -> Result<(), ValueConversionError> {
        let value = allocation
            .value
            .as_ref()
            .ok_or(ValueConversionError::Allocation("value is required"))?;
        let domain = wire_domain(self.domain);
        value.validate(domain).map_err(Box::new)?;
        for entry in &self.entries {
            entry.validate(domain).map_err(Box::new)?;
        }
        for entry in &self.entries {
            if entry.contains(value).map_err(Box::new)? {
                return if entry.assignment_mode == allocation.assignment_mode {
                    Ok(())
                } else {
                    Err(ValueConversionError::Allocation(
                        "assignment mode differs from the first matching declaration",
                    ))
                };
            }
        }
        Err(ValueConversionError::Allocation(
            "value is outside the accepted eligibility",
        ))
    }
}

/// `value_to_wire` encodes a Rust value in the configured or persisted domain.
/// The value's text uses signed decimal for integers and either address family.
/// IPv6 prefixes must include their prefix length and have zero host bits. Narrower consumer
/// policies such as VNI limits remain with the caller.
pub fn value_to_wire<T: ToString>(
    domain: ResourcePoolValueDomain,
    value: &T,
) -> Result<v1::Value, ValueConversionError> {
    let text = value.to_string();
    let value = match domain {
        ResourcePoolValueDomain::Integer => text.parse::<i64>()?.into(),
        ResourcePoolValueDomain::IpAddress => match text.parse::<IpAddr>()? {
            IpAddr::V4(address) => address.into(),
            IpAddr::V6(address) => address.into(),
        },
        ResourcePoolValueDomain::Ipv6Prefix => {
            // `Ipv6Network` also accepts bare addresses as /128, but the
            // consumer must identify its value as a prefix explicitly.
            if !text.contains('/') {
                return Err(ValueConversionError::Wire(Box::new(
                    tonic::Status::invalid_argument("ipv6 prefix length is required"),
                )));
            }
            prefix_value(text.parse::<Ipv6Network>()?)
        }
    };
    value.validate(wire_domain(domain)).map_err(Box::new)?;
    Ok(value)
}

/// `value_from_wire` checks the authoritative domain before parsing the value
/// as the Rust consumer type. For the `IpAddress` domain, `IpAddr` accepts both
/// address families while `Ipv6Addr` accepts only IPv6. Integer
/// narrowing, such as an `i64` value outside `i32`, preserves the parse error
/// in `ValueConversionError::Consumer`. The parsed consumer must also encode
/// back to the same domain and value; conversion rejects types whose parsing
/// or formatting changes either. For example, an address cannot become a
/// `/128` prefix through `Ipv6Network`'s permissive parser.
/// Allocation membership and assignment mode require
/// [`ResourcePoolEligibility::validate_allocation`] with the accepted eligibility;
/// request identity and selection still require the caller's saved-operation checks.
pub fn value_from_wire<T>(
    domain: ResourcePoolValueDomain,
    value: &v1::Value,
) -> Result<T, ValueConversionError>
where
    T: FromStr + ToString,
    T::Err: Error + Send + Sync + 'static,
{
    value.validate(wire_domain(domain)).map_err(Box::new)?;
    let text = match value.kind.as_ref() {
        Some(v1::value::Kind::Integer(value)) => value.to_string(),
        Some(v1::value::Kind::Ipv4(address)) => {
            Ipv4Addr::from(<[u8; 4]>::try_from(address.as_slice())?).to_string()
        }
        Some(v1::value::Kind::Ipv6(address)) => {
            Ipv6Addr::from(<[u8; 16]>::try_from(address.as_slice())?).to_string()
        }
        Some(v1::value::Kind::Ipv6Prefix(prefix)) => format!(
            "{}/{}",
            Ipv6Addr::from(<[u8; 16]>::try_from(prefix.network.as_slice())?),
            prefix.prefix_len
        ),
        None => {
            return Err(ValueConversionError::Wire(Box::new(
                tonic::Status::invalid_argument("value kind is required"),
            )));
        }
    };
    let converted: T = text
        .parse()
        .map_err(|source| ValueConversionError::Consumer {
            rust_type: type_name::<T>(),
            source: Box::new(source),
        })?;
    // `Ipv6Network` accepts a bare address as /128. Parsing alone would let
    // an address allocation become a prefix allocation at the consumer.
    let round_trip =
        value_to_wire(domain, &converted).map_err(|source| ValueConversionError::Consumer {
            rust_type: type_name::<T>(),
            source: Box::new(source),
        })?;
    if round_trip != *value {
        return Err(ValueConversionError::ConsumerValueChanged {
            rust_type: type_name::<T>(),
        });
    }
    Ok(converted)
}

fn wire_domain(domain: ResourcePoolValueDomain) -> v1::ValueDomain {
    match domain {
        ResourcePoolValueDomain::Integer => v1::ValueDomain::Integer,
        ResourcePoolValueDomain::IpAddress => v1::ValueDomain::IpAddress,
        ResourcePoolValueDomain::Ipv6Prefix => v1::ValueDomain::Ipv6Prefix,
    }
}

fn prefix_entry(
    pool_type: ResourcePoolType,
    prefix: &str,
    delegate_prefix_len: Option<u8>,
) -> Result<Entries, ValueConversionError> {
    match pool_type {
        ResourcePoolType::Ipv4 => {
            let network: Ipv4Network = prefix.parse()?;
            Ok(Entries::AddressPrefix(v1::AddressPrefix {
                prefix: Some(Prefix::Ipv4(v1::Ipv4Prefix {
                    network: network.network().octets().to_vec(),
                    prefix_len: network.prefix().into(),
                })),
            }))
        }
        ResourcePoolType::Ipv6 | ResourcePoolType::Ipv6Prefix => {
            let network: Ipv6Network = prefix.parse()?;
            let prefix = v1::Ipv6Prefix {
                network: network.network().octets().to_vec(),
                prefix_len: network.prefix().into(),
            };
            if pool_type == ResourcePoolType::Ipv6 {
                return Ok(Entries::AddressPrefix(v1::AddressPrefix {
                    prefix: Some(Prefix::Ipv6(prefix)),
                }));
            }
            let delegate_prefix_len = delegate_prefix_len.ok_or(
                ValueConversionError::Definition("IPv6 prefix delegation length is required"),
            )?;
            Ok(Entries::DelegatedPrefix(v1::DelegatedPrefix {
                prefix: Some(prefix),
                delegate_prefix_len: delegate_prefix_len.into(),
            }))
        }
        ResourcePoolType::Integer => Err(ValueConversionError::Definition(
            "integer pools require ranges",
        )),
    }
}

fn range_value(pool_type: ResourcePoolType, text: &str) -> Result<v1::Value, ValueConversionError> {
    match pool_type {
        ResourcePoolType::Integer => Ok(parse_integer_range_bound(text)?.into()),
        ResourcePoolType::Ipv4 => Ok(text.parse::<Ipv4Addr>()?.into()),
        ResourcePoolType::Ipv6 => Ok(text.parse::<Ipv6Addr>()?.into()),
        ResourcePoolType::Ipv6Prefix => Ok(prefix_value(text.parse::<Ipv6Network>()?)),
    }
}

fn prefix_value(network: Ipv6Network) -> v1::Value {
    v1::Value {
        kind: Some(v1::value::Kind::Ipv6Prefix(v1::Ipv6Prefix {
            // A range endpoint or allocated prefix must already be aligned.
            // Preserve its IP so the shared validator can reject host bits.
            network: network.ip().octets().to_vec(),
            prefix_len: network.prefix().into(),
        })),
    }
}
