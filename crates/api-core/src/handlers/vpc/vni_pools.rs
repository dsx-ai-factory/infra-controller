// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Compare VNI membership without expanding remote eligibility into inventory.

use carbide_resource_pool::registration;
use carbide_resource_pool_api::v1;
use model::resource_pool::ResourcePool;
use sqlx::PgConnection;

use crate::CarbideError;
use crate::api::Api;

/// `validate_membership` checks destination presence and disjoint VNI membership.
/// Integrated pools use persisted inventory; remote pools use acknowledged ranges.
pub(super) async fn validate_membership(
    api: &Api,
    txn: &mut PgConnection,
    destination: &ResourcePool<i32>,
) -> Result<(), CarbideError> {
    let internal = api.common_pools.ethernet.pool_vpc_vni.as_ref();
    let external = api.common_pools.ethernet.pool_external_vpc_vni.as_ref();
    let internal_membership = remote_membership(api, txn, internal).await?;
    let external_membership = remote_membership(api, txn, external).await?;
    let destination_membership = if destination.name() == internal.name() {
        &internal_membership
    } else {
        &external_membership
    };
    let destination_present = match destination_membership {
        Some(intervals) => !intervals.is_empty(),
        None => db::resource_pool::pool_has_rows(txn, destination.name()).await?,
    };
    if !destination_present {
        return Err(CarbideError::FailedPrecondition(format!(
            "destination pool `{}` has no materialized values or accepted eligibility",
            destination.name(),
        )));
    }
    let overlap = match (internal_membership, external_membership) {
        (None, None) => db::resource_pool::find_pool_overlap(txn, internal, external)
            .await?
            .map(i64::from),
        (Some(first), Some(second)) => first_overlap(first, second),
        (Some(remote), None) => {
            db::resource_pool::find_pool_overlap_with_ranges(txn, external, &remote).await?
        }
        (None, Some(remote)) => {
            db::resource_pool::find_pool_overlap_with_ranges(txn, internal, &remote).await?
        }
    };
    if let Some(vni) = overlap {
        return Err(CarbideError::FailedPrecondition(format!(
            "internal and external VNI pools overlap at VNI `{vni}`",
        )));
    }
    Ok(())
}

/// `remote_membership` returns acknowledged integer intervals for a remote pool.
/// Integrated or unbound pools return `None`, keeping their inventory checks in SQL.
async fn remote_membership(
    api: &Api,
    txn: &mut PgConnection,
    pool: &ResourcePool<i32>,
) -> Result<Option<Vec<(i64, i64)>>, CarbideError> {
    let backend = api
        .resource_pool_registry
        .get_or_load_backend(txn, pool.name())
        .await?;
    if backend.is_none_or(|backend| backend.remote().is_none()) {
        return Ok(None);
    }
    let binding = db::resource_pool::binding::load(&mut *txn, pool.name())
        .await?
        .ok_or_else(|| {
            CarbideError::FailedPrecondition(format!(
                "resource pool `{}` has no binding",
                pool.name()
            ))
        })?;
    let definition = registration::accepted_in_transaction(txn, &binding, None)
        .await?
        .ok_or_else(|| {
            CarbideError::FailedPrecondition(format!(
                "resource pool `{}` has no accepted eligibility",
                pool.name()
            ))
        })?;
    let mut intervals = Vec::new();
    for entry in definition.eligibility {
        if let Some(interval) = integer_interval(entry)? {
            intervals.push(interval);
        }
    }
    Ok(Some(intervals))
}

/// Malformed entries must fail rather than silently remove allowed pool values.
fn invalid_eligibility() -> CarbideError {
    CarbideError::FailedPrecondition(
        "accepted VNI eligibility must contain integer values or ranges".to_owned(),
    )
}

/// `integer_interval` converts a value or half-open range to inclusive endpoints.
/// Empty ranges return `None`; missing or non-integer values fail. Inclusive
/// endpoints represent `i64::MAX` without an overflowing successor.
fn integer_interval(entry: v1::Eligibility) -> Result<Option<(i64, i64)>, CarbideError> {
    let integer = |value: Option<v1::Value>| match value.and_then(|value| value.kind) {
        Some(v1::value::Kind::Integer(value)) => Ok(value),
        _ => Err(invalid_eligibility()),
    };
    match entry.entries {
        Some(v1::eligibility::Entries::Value(value)) => {
            let value = integer(Some(value))?;
            Ok(Some((value, value)))
        }
        Some(v1::eligibility::Entries::Range(range)) => {
            let start = integer(range.start)?;
            let end = integer(range.end)?;
            Ok((start < end).then(|| (start, end - 1)))
        }
        _ => Err(invalid_eligibility()),
    }
}

/// `first_overlap` sorts both interval lists and finds a shared value without
/// expanding either pool into individual values.
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

    /// Conversion preserves exclusive range ends and the largest integer value,
    /// while rejecting malformed or non-integer eligibility.
    #[test]
    fn integer_membership_preserves_exclusive_endpoints_and_maximum_values() {
        scenarios!(run = |entry| integer_interval(entry).map_err(|_| ());
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
            }
        );
    }
}
