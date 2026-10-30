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
use std::time::Duration;

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use chrono::{DateTime, Utc};
use futures::TryFutureExt;
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolValue,
};
use model::resource_pool::{OwnerType, ResourcePool, ValueType};
use sqlx::PgPool;
use uuid::Uuid;

use super::{list, load};
use crate::ConditionalWrite::Applied;
use crate::resource_pool::{allocate, allocate_exact, populate, release, stats};
use crate::test_support::postgres::wait_for_blocked_query;

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
        let AllocationSource::Integrated {
            resource_id,
            value: ResourcePoolValue::Integer(value),
            assignment_mode,
            allocated_at,
        } = &allocation.source
        else {
            panic!("Integrated integer allocation expected");
        };
        let (inventory_id, allocated): (i64, DateTime<Utc>) = sqlx::query_as(
            "SELECT id, allocated FROM resource_pool WHERE name = $1 AND value = $2",
        )
        .bind(inventory.name())
        .bind(value.to_string())
        .fetch_one(&pool)
        .await?;
        assert_eq!(allocation.owner_type, OwnerType::Machine);
        assert_eq!(allocation.owner_id, "owner");
        assert_eq!(*resource_id, inventory_id);
        assert_eq!(allocation.state, AllocationState::Attached);
        assert_eq!(*allocated_at, Some(allocated));
        assert_eq!(
            *assignment_mode,
            Some(if *value == 3 {
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
        .find(|allocation| {
            matches!(
                allocation.source,
                AllocationSource::Integrated { value: ResourcePoolValue::Integer(value), .. }
                    if value == automatic
            )
        })
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
    let remaining: Vec<_> = allocations
        .iter()
        .filter(|allocation| allocation.allocation_id != first.allocation_id)
        .cloned()
        .collect();
    assert_eq!(
        list(&pool, inventory.name(), None, limit).await?,
        remaining,
        "releasing one value preserves the owner's other allocation records"
    );
    allocate_exact(
        &inventory,
        &mut connection,
        OwnerType::Machine,
        "owner",
        automatic,
    )
    .await?;
    let current = list(&pool, inventory.name(), None, limit).await?;
    assert_eq!(current.len(), 3);
    let replacement = current
        .into_iter()
        .find(|allocation| {
            matches!(
                allocation.source,
                AllocationSource::Integrated { value: ResourcePoolValue::Integer(value), .. }
                    if value == automatic
            )
        })
        .unwrap();
    assert_ne!(replacement.allocation_id, first.allocation_id);
    let AllocationSource::Integrated {
        resource_id: original_id,
        ..
    } = first.source
    else {
        panic!("Integrated allocation expected");
    };
    let AllocationSource::Integrated {
        resource_id: replacement_id,
        ..
    } = replacement.source
    else {
        panic!("Integrated allocation expected");
    };
    assert_eq!(replacement_id, original_id, "the inventory row is reused");
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
    assert!(matches!(
        allocations[0].source,
        AllocationSource::Integrated { value: ResourcePoolValue::IpAddress(value), .. }
            if value == address
    ));
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
    let error = allocate_exact(&inventory, &mut connection, OwnerType::Machine, "owner", 1)
        .await
        .expect_err("exact allocation must roll back when its record cannot be written");
    assert!(error.to_string().contains("reject_test_owner"), "{error}");
    assert_eq!(stats(&pool, inventory.name()).await?.used, 0);
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
    // A dependent row makes deleting the receipt fail after `release` updates
    // inventory. Both writes must still roll back on an autocommit connection.
    sqlx::query(
        "CREATE TABLE allocation_reference (
            allocation_id UUID REFERENCES resource_pool_allocations(allocation_id)
        )",
    )
    .execute(&pool)
    .await?;
    sqlx::query("INSERT INTO allocation_reference VALUES ($1)")
        .bind(allocation.allocation_id)
        .execute(&pool)
        .await?;
    let error = release(&inventory, &mut connection, 1, OwnerType::Machine, "owner")
        .await
        .expect_err("allocation record failure must roll back release");
    assert!(
        error
            .to_string()
            .contains("allocation_reference_allocation_id_fkey"),
        "{error}"
    );
    assert_eq!(stats(&pool, inventory.name()).await?.used, 1);
    assert_eq!(
        load(&pool, allocation.allocation_id).await?.as_ref(),
        Some(&allocation)
    );
    sqlx::query("DROP TABLE allocation_reference")
        .execute(&mut *connection)
        .await?;
    assert_eq!(
        release(&inventory, &mut connection, 1, OwnerType::Machine, "owner").await?,
        Applied(())
    );
    assert_eq!(stats(&pool, inventory.name()).await?.free, 1);
    assert_eq!(load(&pool, allocation.allocation_id).await?, None);
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
async fn integrated_release_succeeds_without_an_allocation_record(
    pool: PgPool,
) -> eyre::Result<()> {
    let inventory = ResourcePool::new("missing-record".to_owned(), ValueType::Integer);
    let mut connection = pool.acquire().await?;
    populate(&inventory, &mut connection, vec![1_i64], true).await?;
    allocate_exact(&inventory, &mut connection, OwnerType::Machine, "owner", 1).await?;
    sqlx::query("DELETE FROM resource_pool_allocations WHERE pool_name = $1")
        .bind(inventory.name())
        .execute(&pool)
        .await?;

    assert_eq!(
        release(&inventory, &mut connection, 1, OwnerType::Machine, "owner").await?,
        Applied(())
    );
    assert_eq!(stats(&pool, inventory.name()).await?.used, 0);
    Ok(())
}

#[crate::sqlx_test]
async fn integrated_release_waits_for_reallocation_and_removes_its_record(
    pool: PgPool,
) -> eyre::Result<()> {
    let inventory = ResourcePool::new("reallocated-record".to_owned(), ValueType::Integer);
    let mut connection = pool.acquire().await?;
    populate(&inventory, &mut connection, vec![1_i64], true).await?;
    allocate_exact(&inventory, &mut connection, OwnerType::Machine, "owner", 1).await?;

    // Reusing the same owner means a waiting release still matches the new
    // allocation. Its receipt must be removed along with that inventory claim.
    let mut reallocation = pool.begin().await?;
    assert_eq!(
        release(
            &inventory,
            &mut reallocation,
            1,
            OwnerType::Machine,
            "owner"
        )
        .await?,
        Applied(())
    );
    allocate_exact(
        &inventory,
        &mut reallocation,
        OwnerType::Machine,
        "owner",
        1,
    )
    .await?;
    let reallocation_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *reallocation)
        .await?;
    let release = release(&inventory, &mut connection, 1, OwnerType::Machine, "owner");
    let commit_reallocation = async {
        wait_for_blocked_query(&pool, reallocation_pid, "UPDATE resource_pool").await;
        reallocation.commit().await?;
        Ok::<_, eyre::Report>(())
    };
    let (released, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::try_join!(release.map_err(eyre::Report::from), commit_reallocation)
    })
    .await??;
    assert_eq!(released, Applied(()));
    assert_eq!(stats(&pool, inventory.name()).await?.used, 0);
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM resource_pool_allocations WHERE pool_name = $1")
            .bind(inventory.name())
            .fetch_one(&pool)
            .await?;
    assert_eq!(remaining, 0);
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
                let inventory = ResourcePool::new(pool_name.to_owned(), ValueType::Integer);
                let mut connection = pool.acquire().await?;
                populate(&inventory, &mut connection, vec![1_i64], true).await?;
                allocate_exact(&inventory, &mut connection, OwnerType::Machine, "owner", 1).await?;
                let allocation_id: Uuid = sqlx::query_scalar(
                    "UPDATE resource_pool_allocations
                     SET value = $1, value_type = $2, owner_type = $3
                     WHERE pool_name = $4 RETURNING allocation_id",
                )
                .bind(value)
                .bind(value_type)
                .bind(owner_type)
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
