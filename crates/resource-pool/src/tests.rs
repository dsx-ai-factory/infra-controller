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

use std::net::Ipv4Addr;

use db::ConditionalWrite;
use db::resource_pool::{allocate_exact, populate};
use model::resource_pool::{OwnerType, ResourcePool, ResourcePoolEntryState, ValueType};
use sqlx::types::Json;

use crate::{IntegratedResourcePoolBackend, ResourcePoolBackend, ResourcePoolWithBackend};

#[ctor::ctor(unsafe)]
fn setup_test_logging() {
    carbide_test_support::setup_test_logging("resource-pool");
}

#[carbide_macros::sqlx_test]
async fn integrated_forwards_pool_values_and_owners_in_the_borrowed_transaction(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let descriptor = ResourcePool::new("adapter-pool".to_string(), ValueType::Ipv4);
    let other_descriptor = ResourcePool::new("other-pool".to_string(), ValueType::Ipv4);
    let automatic = Ipv4Addr::new(192, 0, 2, 1);
    let existing = Ipv4Addr::new(192, 0, 2, 2);
    let requested = Ipv4Addr::new(192, 0, 2, 3);

    // Both pools have the same existing SQL allocation. Releasing through the
    // adapter must use the selected pool and leave the other allocation alone.
    let mut txn = database.begin().await?;
    populate(&descriptor, &mut txn, vec![automatic, existing], true).await?;
    populate(&descriptor, &mut txn, vec![requested], false).await?;
    populate(&other_descriptor, &mut txn, vec![existing], true).await?;
    for pool in [&descriptor, &other_descriptor] {
        allocate_exact(
            pool,
            &mut txn,
            OwnerType::IBPartition,
            "existing-owner",
            existing,
        )
        .await?;
    }
    txn.commit().await?;

    let selected_backend = ResourcePoolBackend::Integrated(IntegratedResourcePoolBackend);
    let pool = ResourcePoolWithBackend::new(&descriptor, &selected_backend);
    let ResourcePoolBackend::Integrated(backend) = pool.backend();
    let mut txn = database.begin().await?;
    assert_eq!(
        backend
            .release(
                pool.pool(),
                &mut txn,
                existing,
                OwnerType::IBPartition,
                "existing-owner"
            )
            .await?,
        ConditionalWrite::Applied(())
    );
    assert_eq!(
        backend
            .allocate_exact(
                pool.pool(),
                &mut txn,
                OwnerType::Vpc,
                "exact-owner",
                existing
            )
            .await?,
        existing
    );
    assert_eq!(
        backend
            .allocate(
                pool.pool(),
                &mut txn,
                OwnerType::Machine,
                "automatic-owner",
                None
            )
            .await?,
        automatic
    );
    assert_eq!(
        backend
            .allocate(
                pool.pool(),
                &mut txn,
                OwnerType::NetworkSegment,
                "requested-owner",
                Some(requested),
            )
            .await?,
        requested
    );

    type Entry = (String, String, Json<ResourcePoolEntryState>);
    let entries = "SELECT name, value, state FROM resource_pool
        WHERE name IN ($1, $2) ORDER BY name, value";
    let allocated = |owner_type: OwnerType, owner: &str| {
        Json(ResourcePoolEntryState::Allocated {
            owner: owner.to_string(),
            owner_type: owner_type.to_string(),
        })
    };
    let rows: Vec<Entry> = sqlx::query_as(entries)
        .bind(descriptor.name())
        .bind(other_descriptor.name())
        .fetch_all(&mut *txn)
        .await?;
    assert_eq!(
        rows,
        vec![
            (
                descriptor.name().to_string(),
                automatic.to_string(),
                allocated(OwnerType::Machine, "automatic-owner"),
            ),
            (
                descriptor.name().to_string(),
                existing.to_string(),
                allocated(OwnerType::Vpc, "exact-owner"),
            ),
            (
                descriptor.name().to_string(),
                requested.to_string(),
                allocated(OwnerType::NetworkSegment, "requested-owner"),
            ),
            (
                other_descriptor.name().to_string(),
                existing.to_string(),
                allocated(OwnerType::IBPartition, "existing-owner"),
            ),
        ],
    );

    // The caller's rollback must undo every adapter mutation, including the
    // release of the allocation created through SQL before the adapter existed.
    txn.rollback().await?;
    let rows: Vec<Entry> = sqlx::query_as(entries)
        .bind(descriptor.name())
        .bind(other_descriptor.name())
        .fetch_all(&database)
        .await?;
    assert_eq!(
        rows,
        vec![
            (
                descriptor.name().to_string(),
                automatic.to_string(),
                Json(ResourcePoolEntryState::Free),
            ),
            (
                descriptor.name().to_string(),
                existing.to_string(),
                allocated(OwnerType::IBPartition, "existing-owner"),
            ),
            (
                descriptor.name().to_string(),
                requested.to_string(),
                Json(ResourcePoolEntryState::Free),
            ),
            (
                other_descriptor.name().to_string(),
                existing.to_string(),
                allocated(OwnerType::IBPartition, "existing-owner"),
            ),
        ],
    );
    Ok(())
}
