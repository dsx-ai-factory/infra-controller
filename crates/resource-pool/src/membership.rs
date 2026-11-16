// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Inspect allowed pool values while keeping backend definitions inside the library.

use std::str::FromStr;

use carbide_resource_pool_api::v1;
use db::DatabaseError;
use model::resource_pool::{ResourcePool, ValueType};
use sqlx::PgConnection;
use v1::address_prefix::Prefix;
use v1::eligibility::Entries;

use crate::value::{ValueConversionError, wire_domain};
use crate::{ResourcePoolBackendError, ResourcePoolRegistry, registration};

impl ResourcePoolRegistry {
    /// `has_values` checks whether a pool contains any allowed values, including
    /// allocated and manually assigned values. Integrated reads stored inventory,
    /// including later growth; remote reads its latest acknowledged definition.
    /// An absent Integrated pool returns false. Missing remote acknowledgement,
    /// invalid bindings and malformed definitions are errors. This does not call
    /// the provider, count free values, or prevent later pool growth.
    pub async fn has_values<T>(
        &self,
        txn: &mut PgConnection,
        pool: &ResourcePool<T>,
    ) -> Result<bool, ResourcePoolBackendError>
    where
        T: ToString + FromStr + Send + Sync + 'static,
        T::Err: std::error::Error,
    {
        let Some(definition) = self.accepted_definition(txn, pool.name()).await? else {
            return Ok(db::resource_pool::pool_has_rows(txn, pool.name()).await?);
        };
        for entry in &definition.eligibility {
            if eligibility_has_values(entry)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `find_integer_overlap` returns one value allowed by both integer pools,
    /// regardless of assignment mode or current allocation. Integrated uses all
    /// stored inventory; remote uses acknowledged eligibility. Mixed comparisons
    /// stay in SQL against compact ranges, without copying inventory into Rust.
    /// Non-integer handles, invalid bindings and unacknowledged remote definitions
    /// are errors. An absent pool has no overlap. No provider call or lock on
    /// definitions is taken. The returned value proves the pools overlap; it is
    /// not reserved, and which shared value is returned is unspecified.
    pub async fn find_integer_overlap<T>(
        &self,
        txn: &mut PgConnection,
        first: &ResourcePool<T>,
        second: &ResourcePool<T>,
    ) -> Result<Option<i64>, ResourcePoolBackendError>
    where
        T: ToString + FromStr + Send + Sync + 'static,
        T::Err: std::error::Error,
    {
        if first.value_type != ValueType::Integer || second.value_type != ValueType::Integer {
            return Err(ValueConversionError::Definition("integer pools are required").into());
        }
        let first_ranges = self.integer_membership(txn, first.name()).await?;
        let second_ranges = self.integer_membership(txn, second.name()).await?;
        Ok(match (first_ranges, second_ranges) {
            (None, None) => {
                db::resource_pool::find_integer_pool_overlap(txn, first.name(), second.name())
                    .await?
            }
            (Some(first), Some(second)) => first_overlap(first, second),
            (Some(ranges), None) => {
                db::resource_pool::find_pool_overlap_with_ranges(txn, second.name(), &ranges)
                    .await?
            }
            (None, Some(ranges)) => {
                db::resource_pool::find_pool_overlap_with_ranges(txn, first.name(), &ranges).await?
            }
        })
    }

    /// Read acknowledged remote membership, leaving Integrated inventory in SQL.
    async fn integer_membership(
        &self,
        txn: &mut PgConnection,
        name: &str,
    ) -> Result<Option<Vec<(i64, i64)>>, ResourcePoolBackendError> {
        self.accepted_definition(txn, name)
            .await?
            .map(|definition| {
                definition
                    .eligibility
                    .into_iter()
                    .filter_map(|entry| integer_interval(entry).transpose())
                    .collect::<Result<_, _>>()
            })
            .transpose()
            .map_err(Into::into)
    }

    /// Read and validate acknowledged remote eligibility. None selects Integrated
    /// inventory, including pools absent from the registry. No provider I/O occurs.
    async fn accepted_definition(
        &self,
        txn: &mut PgConnection,
        name: &str,
    ) -> Result<Option<v1::DefineRequest>, ResourcePoolBackendError> {
        let Some(resolved) = self.get(name) else {
            // Pools grown after startup still need validation of the saved binding.
            self.get_or_load_backend(txn, name).await?;
            return Ok(None);
        };
        if resolved.backend.remote().is_none() {
            return Ok(None);
        }
        let definition = registration::accepted_in_transaction(txn, &resolved.binding, None)
            .await?
            .ok_or_else(|| {
                DatabaseError::FailedPrecondition(format!(
                    "resource pool `{name}` has no acknowledged eligibility",
                ))
            })?;
        if definition.eligibility.is_empty() {
            return Err(invalid_membership().into());
        }
        // Validate every saved declaration before a positive presence result
        // can hide malformed entries later in the definition.
        for entry in &definition.eligibility {
            entry
                .validate(wire_domain(resolved.binding.value_domain))
                .map_err(|error| ValueConversionError::Wire(Box::new(error)))?;
        }
        Ok(Some(definition))
    }
}

/// Check one validated declaration without enumerating its values.
fn eligibility_has_values(entry: &v1::Eligibility) -> Result<bool, ValueConversionError> {
    Ok(match entry.entries.as_ref() {
        // Asking about the start preserves empty and reversed ranges.
        Some(Entries::Range(range)) => entry
            .contains(range.start.as_ref().ok_or_else(invalid_membership)?)
            .map_err(|error| ValueConversionError::Wire(Box::new(error)))?,
        Some(Entries::AddressPrefix(prefix)) => {
            // IPv4 excludes the broadcast address, leaving a /32 empty.
            // IPv6 includes every address in its prefix.
            !matches!(&prefix.prefix, Some(Prefix::Ipv4(prefix)) if prefix.prefix_len == 32)
        }
        Some(Entries::Value(_) | Entries::DelegatedPrefix(_)) => true,
        None => return Err(invalid_membership()),
    })
}

/// Malformed entries must not silently remove values from membership checks.
fn invalid_membership() -> ValueConversionError {
    ValueConversionError::Definition("pool membership entry is incomplete or invalid")
}

/// Convert integer membership into inclusive endpoints without expanding ranges.
/// Empty ranges disappear; point values can include i64::MAX without overflow.
fn integer_interval(entry: v1::Eligibility) -> Result<Option<(i64, i64)>, ValueConversionError> {
    let integer = |value: Option<v1::Value>| match value.and_then(|value| value.kind) {
        Some(v1::value::Kind::Integer(value)) => Ok(value),
        _ => Err(invalid_membership()),
    };
    match entry.entries {
        Some(Entries::Value(value)) => {
            let value = integer(Some(value))?;
            Ok(Some((value, value)))
        }
        Some(Entries::Range(range)) => {
            let start = integer(range.start)?;
            let end = integer(range.end)?;
            Ok((start < end).then(|| (start, end - 1)))
        }
        _ => Err(invalid_membership()),
    }
}

/// Compare sorted intervals in one pass, preserving gaps and inclusive endpoints.
fn first_overlap(mut first: Vec<(i64, i64)>, mut second: Vec<(i64, i64)>) -> Option<i64> {
    first.sort_unstable();
    second.sort_unstable();
    let (mut left, mut right) = (first.iter(), second.iter());
    let (mut a, mut b) = (left.next()?, right.next()?);
    loop {
        let start = a.0.max(b.0);
        if start <= a.1.min(b.1) {
            return Some(start);
        }
        if a.1 < b.1 {
            a = left.next()?;
        } else {
            b = right.next()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    /// Empty integer ranges and IPv4 broadcast exclusion differ from a valid
    /// IPv6 singleton; presence must follow eligibility rather than entry count.
    #[test]
    fn eligibility_presence_respects_empty_ranges_and_address_families() {
        scenarios!(run = |entries| eligibility_has_values(&v1::Eligibility {
            assignment_mode: v1::AssignmentMode::Automatic as i32,
            entries: Some(entries),
        }).map_err(drop);
            "eligible values" {
                Entries::Range(v1::ValueRange { start: Some(1_i64.into()), end: Some(1_i64.into()) }) => Yields(false),
                Entries::Range(v1::ValueRange { start: Some(1_i64.into()), end: Some(2_i64.into()) }) => Yields(true),
                Entries::AddressPrefix(v1::AddressPrefix { prefix: Some(Prefix::Ipv4(v1::Ipv4Prefix { network: vec![0; 4], prefix_len: 32 })) }) => Yields(false),
                Entries::AddressPrefix(v1::AddressPrefix { prefix: Some(Prefix::Ipv4(v1::Ipv4Prefix { network: vec![0; 4], prefix_len: 31 })) }) => Yields(true),
                Entries::AddressPrefix(v1::AddressPrefix { prefix: Some(Prefix::Ipv6(v1::Ipv6Prefix { network: vec![0; 16], prefix_len: 128 })) }) => Yields(true),
            }
        );
    }

    /// Conversion preserves exclusive range ends and the largest integer value,
    /// while rejecting malformed or non-integer eligibility.
    #[test]
    fn integer_membership_preserves_exclusive_endpoints_and_maximum_values() {
        scenarios!(run = |entry| integer_interval(entry).map_err(drop);
            "integer membership" {
                v1::Eligibility { entries: Some(v1::eligibility::Entries::Value(i64::MAX.into())), ..Default::default() }
                    => Yields(Some((i64::MAX, i64::MAX))),
                v1::Eligibility { entries: Some(v1::eligibility::Entries::Range(v1::ValueRange { start: Some(7_i64.into()), end: Some(9_i64.into()) })), ..Default::default() }
                    => Yields(Some((7, 8))),
                v1::Eligibility { entries: Some(v1::eligibility::Entries::Range(v1::ValueRange { start: Some(9_i64.into()), end: Some(7_i64.into()) })), ..Default::default() }
                    => Yields(None),
                v1::Eligibility { entries: Some(v1::eligibility::Entries::Value(std::net::Ipv4Addr::LOCALHOST.into())), ..Default::default() }
                    => Fails,
                v1::Eligibility::default() => Fails,
            }
        );
    }

    /// Membership intersection must work for adjacent ranges, empty pools and
    /// either input arriving out of order.
    #[test]
    fn interval_overlap_finds_shared_membership() {
        value_scenarios!(run = |(first, second)| first_overlap(first, second);
            "membership intersections" {
                (vec![(7, 8)], vec![(9, 10)]) => None,
                (vec![(7, 8)], vec![(8, 10)]) => Some(8),
                (vec![(20, 30), (7, 8)], vec![(7, 7)]) => Some(7),
                (vec![(1, 1)], vec![(5, 5), (1, 1)]) => Some(1),
                (vec![], vec![(7, 8)]) => None,
                // Both inputs must advance before finding their shared value.
                (vec![(1, 1), (5, 6)], vec![(2, 2), (6, 7)]) => Some(6),
            }
        );
    }
}
