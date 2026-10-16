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

use std::net::IpAddr;
use std::num::NonZeroU16;

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use chrono::{DateTime, Utc};
use config_version::ConfigVersion;
use futures::TryFutureExt;
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolValue,
};
use model::resource_pool::{OwnerType, ResourcePool, ResourcePoolEntryState, ValueType};
use sqlx::PgPool;
use uuid::Uuid;

use super::{list, load};
use crate::ConditionalWrite::Applied;
use crate::resource_pool::{allocate, allocate_exact, populate, release, stats};

#[crate::sqlx_test]
async fn integrated_helpers_record_each_allocation(pool: PgPool) -> eyre::Result<()> {
    let inventory = ResourcePool::new("integrated-grants".to_owned(), ValueType::Integer);
    let limit = NonZeroU16::new(10).unwrap();
    let mut connection = pool.acquire().await?;
    populate(&inventory, &mut connection, vec![1_i64, 2], true).await?;
    populate(&inventory, &mut connection, vec![3], false).await?;
    assert!(list(&pool, inventory.name(), None, limit).await?.is_empty());

    let automatic = allocate(
        &inventory,
        &mut connection,
        OwnerType::Machine,
        "owner",
        None,
    )
    .await?;
    allocate(
        &inventory,
        &mut connection,
        OwnerType::Machine,
        "owner",
        Some(3),
    )
    .await?;
    let exact = if automatic == 1 { 2 } else { 1 };
    allocate_exact(
        &inventory,
        &mut connection,
        OwnerType::Machine,
        "owner",
        exact,
    )
    .await?;

    let allocations = list(&pool, inventory.name(), None, limit).await?;
    assert_eq!(
        allocations.len(),
        3,
        "one owner may hold several values in a pool"
    );
    for allocation in &allocations {
        let ResourcePoolValue::Integer(value) = allocation.value else {
            panic!("integer allocation expected");
        };
        let (resource_id, allocated): (i64, DateTime<Utc>) = sqlx::query_as(
            "SELECT id, allocated FROM resource_pool WHERE name = $1 AND value = $2",
        )
        .bind(inventory.name())
        .bind(value.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(allocation.owner_type, OwnerType::Machine);
        assert_eq!(allocation.owner_id, "owner");
        assert_eq!(
            allocation.source,
            AllocationSource::Integrated { resource_id }
        );
        assert_eq!(allocation.state, AllocationState::Attached);
        assert_eq!(allocation.allocated_at, Some(allocated));
        assert_eq!(
            allocation.assignment_mode,
            Some(if value == 3 {
                AssignmentMode::Manual
            } else {
                AssignmentMode::Automatic
            })
        );
        assert_eq!(
            load(&pool, allocation.allocation_id).await?.as_ref(),
            Some(allocation)
        );
    }

    let first = allocations
        .iter()
        .find(|allocation| allocation.value == ResourcePoolValue::Integer(automatic))
        .unwrap();
    assert_eq!(
        release(
            &inventory,
            &mut connection,
            automatic,
            OwnerType::Machine,
            "owner"
        )
        .await?,
        Applied(())
    );
    assert_eq!(load(&pool, first.allocation_id).await?, None);
    allocate_exact(
        &inventory,
        &mut connection,
        OwnerType::Machine,
        "owner",
        automatic,
    )
    .await?;
    let replacement = list(&pool, inventory.name(), None, limit)
        .await?
        .into_iter()
        .find(|allocation| allocation.value == first.value)
        .unwrap();
    assert_ne!(replacement.allocation_id, first.allocation_id);
    assert_eq!(
        replacement.source, first.source,
        "the inventory row is reused for a new allocation"
    );
    assert_eq!(replacement.owner_id, first.owner_id);
    Ok(())
}

#[crate::sqlx_test]
async fn ipv4_tagged_ip_pools_preserve_ipv6_allocations(pool: PgPool) -> eyre::Result<()> {
    // Existing IpAddr handles can store either address family under the ipv4 tag.
    let inventory = ResourcePool::<IpAddr>::new("ip-addresses".to_owned(), ValueType::Ipv4);
    let address: IpAddr = "2001:db8::7".parse()?;
    let mut connection = pool.acquire().await?;
    populate(&inventory, &mut connection, vec![address], true).await?;
    let allocated = allocate(
        &inventory,
        &mut connection,
        OwnerType::Machine,
        "owner",
        None,
    )
    .await?;
    assert_eq!(allocated, address);

    let allocations = list(&pool, inventory.name(), None, NonZeroU16::new(1).unwrap()).await?;
    assert_eq!(allocations.len(), 1);
    assert_eq!(allocations[0].value, ResourcePoolValue::IpAddress(address));
    assert_eq!(
        load(&pool, allocations[0].allocation_id).await?.as_ref(),
        Some(&allocations[0])
    );
    Ok(())
}

#[crate::sqlx_test]
async fn integrated_allocation_record_failure_rolls_back_autocommit_inventory(
    pool: PgPool,
) -> eyre::Result<()> {
    let inventory = ResourcePool::new("atomic-grant".to_owned(), ValueType::Integer);
    let mut connection = pool.acquire().await?;
    populate(&inventory, &mut connection, vec![1_i64], true).await?;
    sqlx::query(
        "ALTER TABLE resource_pool_allocations
         ADD CONSTRAINT reject_test_owner CHECK (owner_id <> 'owner')",
    )
    .execute(&pool)
    .await?;

    let error = allocate(
        &inventory,
        &mut connection,
        OwnerType::Machine,
        "owner",
        None,
    )
    .await
    .expect_err("allocation record failure must reject the allocation statement");
    assert!(error.to_string().contains("reject_test_owner"), "{error}");
    assert_eq!(stats(&pool, inventory.name()).await?.used, 0);
    assert!(
        list(&pool, inventory.name(), None, NonZeroU16::new(1).unwrap())
            .await?
            .is_empty()
    );
    sqlx::query("ALTER TABLE resource_pool_allocations DROP CONSTRAINT reject_test_owner")
        .execute(&pool)
        .await?;

    allocate(
        &inventory,
        &mut connection,
        OwnerType::Machine,
        "owner",
        None,
    )
    .await?;
    let allocation = list(&pool, inventory.name(), None, NonZeroU16::new(1).unwrap())
        .await?
        .remove(0);
    // Fail the allocation record deletion after `release` updates the inventory.
    sqlx::raw_sql(
        "CREATE FUNCTION reject_test_allocation_delete() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'test allocation record deletion failed'; END $$;
         CREATE TRIGGER reject_test_allocation_delete BEFORE DELETE ON resource_pool_allocations
         FOR EACH ROW EXECUTE FUNCTION reject_test_allocation_delete();",
    )
    .execute(&pool)
    .await?;
    let error = release(&inventory, &mut connection, 1, OwnerType::Machine, "owner")
        .await
        .expect_err("allocation record failure must reject the release statement");
    assert!(
        error
            .to_string()
            .contains("test allocation record deletion failed"),
        "{error}"
    );
    assert_eq!(stats(&pool, inventory.name()).await?.used, 1);
    assert_eq!(
        load(&pool, allocation.allocation_id).await?,
        Some(allocation)
    );
    Ok(())
}

#[crate::sqlx_test]
async fn integrated_transaction_rollback_restores_inventory_and_allocation_record(
    pool: PgPool,
) -> eyre::Result<()> {
    let inventory = ResourcePool::new("rollback-grant".to_owned(), ValueType::Integer);
    let limit = NonZeroU16::new(1).unwrap();
    let mut connection = pool.acquire().await?;
    populate(&inventory, &mut connection, vec![1_i64], true).await?;
    let mut allocation = pool.begin().await?;
    allocate_exact(&inventory, &mut allocation, OwnerType::Machine, "owner", 1).await?;
    let rolled_back = list(allocation.as_mut(), inventory.name(), None, limit)
        .await?
        .remove(0);
    assert_eq!(load(&pool, rolled_back.allocation_id).await?, None);
    allocation.rollback().await?;
    assert_eq!(stats(&pool, inventory.name()).await?.used, 0);
    assert_eq!(load(&pool, rolled_back.allocation_id).await?, None);

    allocate_exact(&inventory, &mut connection, OwnerType::Machine, "owner", 1).await?;
    let retained = list(&pool, inventory.name(), None, limit).await?.remove(0);
    let mut deletion = pool.begin().await?;
    assert_eq!(
        release(&inventory, &mut deletion, 1, OwnerType::Machine, "owner").await?,
        Applied(())
    );
    assert_eq!(load(deletion.as_mut(), retained.allocation_id).await?, None);
    deletion.rollback().await?;
    assert_eq!(stats(&pool, inventory.name()).await?.used, 1);
    assert_eq!(load(&pool, retained.allocation_id).await?, Some(retained));
    Ok(())
}

#[crate::sqlx_test]
async fn allocated_inserts_preserve_typed_values_and_unknown_metadata(
    pool: PgPool,
) -> eyre::Result<()> {
    check_cases_async(
        [
            (
                "signed integer",
                ValueType::Integer,
                i64::MIN.to_string(),
                ResourcePoolValue::Integer(i64::MIN),
            ),
            (
                "IPv4 address",
                ValueType::Ipv4,
                "192.0.2.7".to_owned(),
                ResourcePoolValue::IpAddress("192.0.2.7".parse()?),
            ),
            (
                "IPv6 address",
                ValueType::Ipv6,
                "2001:db8::7".to_owned(),
                ResourcePoolValue::IpAddress("2001:db8::7".parse()?),
            ),
            (
                "IPv6 prefix",
                ValueType::Ipv6Prefix,
                "2001:db8::/64".to_owned(),
                ResourcePoolValue::Ipv6Prefix("2001:db8::/64".parse()?),
            ),
        ]
        .map(|(scenario, value_type, value, expected)| Case {
            scenario,
            input: (scenario, value_type, value),
            expect: Yields(expected),
        }),
        |(pool_name, value_type, value)| {
            let pool = &pool;
            async move {
                let resource_id: i64 = sqlx::query_scalar(
                    "INSERT INTO resource_pool (
                         name, value, value_type, state, state_version, auto_assign, allocated
                     ) VALUES ($1, $2, $3, $4, $5, NULL, NULL) RETURNING id",
                )
                .bind(pool_name)
                .bind(value)
                .bind(value_type)
                .bind(sqlx::types::Json(ResourcePoolEntryState::Allocated {
                    owner: "owner".to_owned(),
                    owner_type: OwnerType::Machine.to_string(),
                }))
                .bind(ConfigVersion::initial())
                .fetch_one(pool)
                .await?;
                let allocation = list(pool, pool_name, None, NonZeroU16::new(1).unwrap())
                    .await?
                    .remove(0);
                assert_eq!(
                    allocation.source,
                    AllocationSource::Integrated { resource_id }
                );
                assert_eq!(allocation.assignment_mode, None);
                assert_eq!(allocation.allocated_at, None);
                assert_eq!(allocation.state, AllocationState::Attached);
                sqlx::query("DELETE FROM resource_pool WHERE id = $1")
                    .bind(resource_id)
                    .execute(pool)
                    .await?;
                assert_eq!(load(pool, allocation.allocation_id).await?, None);
                Ok::<_, eyre::Report>(allocation.value)
            }
            .map_err(|error| format!("{error:#}"))
        },
    )
    .await;
    Ok(())
}

#[crate::sqlx_test]
async fn integrated_record_identity_changes_only_for_a_new_allocation(
    pool: PgPool,
) -> eyre::Result<()> {
    let inventory = ResourcePool::new("original-pool".to_owned(), ValueType::Integer);
    let mut connection = pool.acquire().await?;
    populate(&inventory, &mut connection, vec![1_i64], true).await?;
    allocate_exact(&inventory, &mut connection, OwnerType::Machine, "owner", 1).await?;
    let original = list(&pool, inventory.name(), None, NonZeroU16::new(1).unwrap())
        .await?
        .remove(0);
    let AllocationSource::Integrated { resource_id } = original.source else {
        panic!("integrated allocation expected");
    };
    sqlx::query(
        "UPDATE resource_pool
         SET name = 'renamed-pool', value = '192.0.2.1', value_type = 'ipv4', auto_assign = false
         WHERE id = $1",
    )
    .bind(resource_id)
    .execute(&pool)
    .await?;
    let changed = load(&pool, original.allocation_id)
        .await?
        .expect("metadata changes preserve the allocation record");
    assert_eq!(changed.pool_name, "renamed-pool");
    assert_eq!(
        changed.value,
        ResourcePoolValue::IpAddress("192.0.2.1".parse()?)
    );
    assert_eq!(changed.assignment_mode, Some(AssignmentMode::Manual));
    assert_eq!(changed.allocated_at, original.allocated_at);
    assert_eq!(changed.recorded_at, original.recorded_at);
    sqlx::query("UPDATE resource_pool SET state = state, allocated = allocated WHERE id = $1")
        .bind(resource_id)
        .execute(&pool)
        .await?;
    assert_eq!(load(&pool, original.allocation_id).await?, Some(changed));

    let mut previous_id = original.allocation_id;
    for (scenario, state, allocated) in [
        (
            "different owner",
            ResourcePoolEntryState::Allocated {
                owner: "replacement".to_owned(),
                owner_type: OwnerType::Machine.to_string(),
            },
            original.allocated_at,
        ),
        (
            "different allocation time",
            ResourcePoolEntryState::Allocated {
                owner: "replacement".to_owned(),
                owner_type: OwnerType::Machine.to_string(),
            },
            None,
        ),
    ] {
        sqlx::query("UPDATE resource_pool SET state = $1, allocated = $2 WHERE id = $3")
            .bind(sqlx::types::Json(&state))
            .bind(allocated)
            .bind(resource_id)
            .execute(&pool)
            .await?;
        assert_eq!(load(&pool, previous_id).await?, None, "{scenario}");
        let replacement = list(&pool, "renamed-pool", None, NonZeroU16::new(1).unwrap())
            .await?
            .remove(0);
        assert_ne!(replacement.allocation_id, previous_id, "{scenario}");
        assert_eq!(replacement.owner_id, "replacement", "{scenario}");
        assert_eq!(replacement.allocated_at, allocated, "{scenario}");
        previous_id = replacement.allocation_id;
    }
    Ok(())
}

#[crate::sqlx_test]
async fn allocation_pages_are_bounded_ordered_and_filtered_by_pool(
    pool: PgPool,
) -> eyre::Result<()> {
    let inventory = ResourcePool::new("paged-pool".to_owned(), ValueType::Integer);
    let other = ResourcePool::new("other-pool".to_owned(), ValueType::Integer);
    let mut connection = pool.acquire().await?;
    populate(&inventory, &mut connection, vec![4_i64, 1, 3, 2], true).await?;
    populate(&other, &mut connection, vec![0_i64], true).await?;
    for (resource_pool, value) in [
        (&inventory, 4_i64),
        (&inventory, 1),
        (&inventory, 3),
        (&inventory, 2),
        (&other, 0),
    ] {
        allocate_exact(
            resource_pool,
            &mut connection,
            OwnerType::Machine,
            "owner",
            value,
        )
        .await?;
        sqlx::query(
            "UPDATE resource_pool_allocations SET allocation_id = $1
             WHERE pool_name = $2 AND value = $3",
        )
        .bind(Uuid::from_u128(value as u128))
        .bind(resource_pool.name())
        .bind(value.to_string())
        .execute(&pool)
        .await?;
    }
    let limit = NonZeroU16::new(2).unwrap();
    let first = list(&pool, inventory.name(), None, limit).await?;
    assert_eq!(
        first
            .iter()
            .map(|allocation| allocation.allocation_id.as_u128())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let second = list(&pool, inventory.name(), Some(first[1].allocation_id), limit).await?;
    assert_eq!(
        second
            .iter()
            .map(|allocation| allocation.allocation_id.as_u128())
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(
        list(
            &pool,
            inventory.name(),
            Some(second[1].allocation_id),
            limit
        )
        .await?
        .is_empty()
    );
    assert_eq!(load(&pool, Uuid::from_u128(5)).await?, None);
    Ok(())
}

#[crate::sqlx_test]
async fn shared_reads_reject_invalid_stored_values_and_owners(pool: PgPool) -> eyre::Result<()> {
    check_cases_async(
        [
            (
                "malformed integer",
                ValueType::Integer,
                "not-an-integer",
                "machine",
                "value",
            ),
            (
                "malformed IP address",
                ValueType::Ipv4,
                "not-an-ip-address",
                "machine",
                "value",
            ),
            (
                "unknown owner type",
                ValueType::Integer,
                "1",
                "unknown",
                "owner_type",
            ),
        ]
        .map(|(scenario, value_type, value, owner_type, column)| Case {
            scenario,
            input: (scenario, value_type, value, owner_type, column),
            expect: Yields(()),
        }),
        |(pool_name, value_type, value, owner_type, column)| {
            let pool = &pool;
            async move {
                sqlx::query(
                    "INSERT INTO resource_pool (name, value, value_type, state, state_version)
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(pool_name)
                .bind(value)
                .bind(value_type)
                .bind(sqlx::types::Json(ResourcePoolEntryState::Allocated {
                    owner: "owner".to_owned(),
                    owner_type: owner_type.to_owned(),
                }))
                .bind(ConfigVersion::initial())
                .execute(pool)
                .await?;
                let allocation_id: Uuid = sqlx::query_scalar(
                    "SELECT allocation_id FROM resource_pool_allocations WHERE pool_name = $1",
                )
                .bind(pool_name)
                .fetch_one(pool)
                .await?;
                for (operation, error) in [
                    (
                        "load",
                        load(pool, allocation_id)
                            .await
                            .expect_err("invalid stored field"),
                    ),
                    (
                        "list",
                        list(pool, pool_name, None, NonZeroU16::new(1).unwrap())
                            .await
                            .expect_err("invalid stored field"),
                    ),
                ] {
                    let crate::DatabaseError::Sqlx(error) = error else {
                        panic!("{operation}: expected a stored field decode error: {error}");
                    };
                    assert!(
                        matches!(
                            &error.source,
                            sqlx::Error::ColumnDecode { index, .. } if index == column
                        ),
                        "{operation}: {error:?}"
                    );
                }
                Ok::<_, eyre::Report>(())
            }
            .map_err(|error| format!("{error:#}"))
        },
    )
    .await;
    Ok(())
}
