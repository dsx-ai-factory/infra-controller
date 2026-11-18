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
use db::resource_pool::{
    ResourcePoolAllocationNotOwned, allocate_exact, binding, populate, remote,
};
use model::resource_pool::allocation::{AllocationState, AssignmentMode, ResourcePoolValue};
use model::resource_pool::remote::{
    AllocationAssignment, AllocationOwner, AllocationResult, AllocationSelection,
    CompletionAuthority, RemoteAllocationRequest, RemotePoolIdentity,
};
use model::resource_pool::{
    OwnerType, ResourcePool, ResourcePoolBackendKind, ResourcePoolEntryState,
    ResourcePoolSelection, ResourcePoolValueDomain, ValueType,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    IntegratedResourcePoolBackend, ResourcePoolBackend, ResourcePoolBackendError,
    ResourcePoolRegistry, ResourcePoolRegistryError, ResourcePoolWithBackend,
};

#[ctor::ctor(unsafe)]
fn setup_test_logging() {
    carbide_test_support::setup_test_logging("resource-pool");
}

/// Integrated adapters preserve pool, value, and owner identity in the caller's
/// transaction, including owned lookup and release.
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
    // backend must use the selected pool and leave the other allocation alone.
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
    let backend = selected_backend.require_integrated()?;
    let mut txn = database.begin().await?;
    assert_eq!(
        backend
            .release(
                &descriptor,
                &mut txn,
                existing,
                OwnerType::Vpc,
                "existing-owner"
            )
            .await?,
        ConditionalWrite::NotApplied(ResourcePoolAllocationNotOwned)
    );
    let registry = ResourcePoolRegistry::default();
    let owned = registry
        .find_owned(
            &descriptor,
            &mut txn,
            OwnerType::IBPartition,
            "existing-owner",
            "address",
        )
        .await?
        .expect("existing owner has its IPv4 allocation");
    assert_eq!(*owned.value(), existing);
    assert_eq!(
        owned.release(&mut txn).await?,
        ConditionalWrite::Applied(())
    );
    assert_eq!(
        backend
            .allocate_exact(
                &descriptor,
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
                &descriptor,
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
                &descriptor,
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

    // The caller's rollback must undo every backend mutation, including the
    // release of the allocation created directly through SQL.
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

/// Inventory lookup must precede binding validation so callers can perform their
/// entity checks. An invalid binding still prevents release and keeps ownership.
#[carbide_macros::sqlx_test]
async fn uncached_integrated_ownership_checks_its_binding_only_on_release(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = ResourcePoolRegistry::default();
    let descriptor = ResourcePool::new("uncached-integrated".to_owned(), ValueType::Integer);
    let mut txn = database.begin().await?;
    populate(&descriptor, &mut txn, vec![42], true).await?;
    allocate_exact(&descriptor, &mut txn, OwnerType::Vpc, "vpc-1", 42).await?;
    // Deliberately pair local inventory with a remote binding so lookup must
    // defer the resulting validation failure until release.
    binding::bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: descriptor.name().to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("fabric".to_owned()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: "provider-a".to_owned(),
            remote_pool: "provider-pool".to_owned(),
        },
    )
    .await?;
    txn.commit().await?;

    // This inconsistent binding must not prevent the caller from checking its
    // entity after lookup has locked the Integrated inventory.
    let mut txn = database.begin().await?;
    let owned = registry
        .find_owned(&descriptor, &mut txn, OwnerType::Vpc, "vpc-1", "vni")
        .await?
        .expect("Integrated ownership remains available before binding validation");
    assert_eq!(*owned.value(), 42);
    assert!(matches!(
        owned.release(&mut txn).await,
        Err(ResourcePoolBackendError::Registry(
            ResourcePoolRegistryError::UnsupportedAuthority { name }
        )) if name == descriptor.name()
    ));
    txn.commit().await?;
    let state: Json<ResourcePoolEntryState> =
        sqlx::query_scalar("SELECT state FROM resource_pool WHERE name = $1")
            .bind(descriptor.name())
            .fetch_one(&database)
            .await?;
    assert_eq!(
        state.0,
        ResourcePoolEntryState::Allocated {
            owner: "vpc-1".to_owned(),
            owner_type: OwnerType::Vpc.to_string(),
        }
    );
    Ok(())
}

/// An empty local pool does not prove absence when another replica saved a remote
/// binding. Fail visibly and keep the attached receipt available for release.
#[carbide_macros::sqlx_test]
async fn uncached_remote_ownership_returns_an_error_and_preserves_attachment(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = ResourcePoolRegistry::default();
    let descriptor = ResourcePool::<i32>::new("uncached-remote".to_owned(), ValueType::Integer);
    // A synthetic attachment isolates registry selection from registration and
    // provider behavior. Its remote binding has no runtime in this registry.
    let mut txn = database.begin().await?;
    let binding = binding::bind_or_verify(
        &mut txn,
        &ResourcePoolSelection {
            pool_name: descriptor.name().to_owned(),
            value_domain: ResourcePoolValueDomain::Integer,
            backend_name: Some("fabric".to_owned()),
            backend_kind: ResourcePoolBackendKind::Grpc,
            authority_id: "provider-a".to_owned(),
            remote_pool: "provider-pool".to_owned(),
        },
    )
    .await?;
    let request = RemoteAllocationRequest {
        request_id: Uuid::new_v4(),
        binding_id: binding.binding_id,
        pool: RemotePoolIdentity {
            authority_id: binding.authority_id,
            pool_id: binding.remote_pool,
            logical_pool: binding.pool_name,
        },
        owner: AllocationOwner {
            owner_type: OwnerType::Vpc,
            owner_id: "vpc-1".to_owned(),
            slot: "vni".to_owned(),
            generation: 0,
        },
        eligibility_revision: "initial".to_owned(),
        selection: AllocationSelection::Automatic,
    };
    let authority = CompletionAuthority::Controller {
        desire_id: Uuid::new_v4(),
    };
    remote::begin(&mut txn, &request, &authority).await?;
    txn.commit().await?;

    let assignment = AllocationAssignment {
        allocation_id: "remote-allocation-1".to_owned(),
        value: ResourcePoolValue::Integer(42),
        assignment_mode: AssignmentMode::Automatic,
        route_target_policy: None,
    };
    let mut txn = database.begin().await?;
    assert_eq!(
        remote::record_result(
            &mut txn,
            request.request_id,
            &AllocationResult::Allocated(assignment.clone()),
        )
        .await?,
        ConditionalWrite::Applied(())
    );
    assert_eq!(
        remote::attach(&mut txn, request.request_id, &authority, &assignment).await?,
        ConditionalWrite::Applied(())
    );
    txn.commit().await?;
    let attached = remote::load(&database, request.request_id)
        .await?
        .expect("remote allocation was saved");
    assert_eq!(attached.state, AllocationState::Attached);

    let mut txn = database.begin().await?;
    assert!(matches!(
        registry
            .find_owned(
                &descriptor,
                &mut txn,
                request.owner.owner_type,
                &request.owner.owner_id,
                &request.owner.slot,
            )
            .await,
        Err(ResourcePoolBackendError::Registry(
            ResourcePoolRegistryError::UnsupportedAuthority { name }
        )) if name == descriptor.name()
    ));
    txn.commit().await?;
    assert_eq!(
        remote::load(&database, request.request_id).await?,
        Some(attached)
    );
    Ok(())
}

/// Integrated preparation leaves inventory free without querying the caller's
/// transaction or opening a connection. Reservation belongs to the entity
/// transaction and its rollback.
#[carbide_macros::sqlx_test]
async fn shared_allocation_defers_integrated_reservation_until_the_entity_transaction(
    database: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let descriptor = ResourcePool::<i32>::new("prepared-pool".to_owned(), ValueType::Integer);
    let backend = ResourcePoolBackend::Integrated(IntegratedResourcePoolBackend);
    let selected = ResourcePoolWithBackend {
        pool: &descriptor,
        backend: &backend,
    };
    let mut txn = database.begin().await?;
    populate(&descriptor, &mut txn, vec![42], true).await?;
    populate(&descriptor, &mut txn, vec![43], false).await?;
    txn.commit().await?;

    // An aborted caller transaction makes preparation SQL fail immediately. The
    // cached Integrated selection must prepare automatic and requested values.
    let mut txn = database.begin().await?;
    let error = sqlx::query("SELECT 1 / 0")
        .execute(&mut *txn)
        .await
        .expect_err("division by zero aborts the preparation transaction");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("22012"),
    );
    let prepared = selected
        .prepare_request(
            &mut txn,
            OwnerType::NetworkSegment,
            "segment-1",
            "vni",
            None,
            CompletionAuthority::Controller {
                desire_id: Uuid::new_v4(),
            },
        )
        .await?;
    let requested = selected
        .prepare_request(
            &mut txn,
            OwnerType::NetworkSegment,
            "segment-1",
            "vni",
            Some(43),
            CompletionAuthority::Controller {
                desire_id: Uuid::new_v4(),
            },
        )
        .await?;
    txn.rollback().await?;

    // The separate closed pool detects connections opened by resolution,
    // foreground preparation, or cancellation.
    let closed =
        PgPoolOptions::new().connect_lazy_with(database.connect_options().as_ref().clone());
    closed.close().await;
    let allocations = [
        // Controllers prepare and resolve before the entity transaction.
        (
            "controller automatic",
            prepared.resolve(&closed, &CancellationToken::new()).await?,
            42,
        ),
        // Ordinary callers receive the same owned allocation in one call.
        (
            "foreground automatic",
            selected
                .prepare_allocation(
                    &closed,
                    OwnerType::NetworkSegment,
                    "segment-1",
                    "vni",
                    None,
                    &CancellationToken::new(),
                )
                .await?,
            42,
        ),
        (
            "controller requested",
            requested
                .resolve(&closed, &CancellationToken::new())
                .await?,
            43,
        ),
    ];
    let state_query = "SELECT state FROM resource_pool WHERE name = $1 AND value = $2";

    for (scenario, allocation, expected) in allocations {
        let before: Json<ResourcePoolEntryState> = sqlx::query_scalar(state_query)
            .bind(descriptor.name())
            .bind(expected.to_string())
            .fetch_one(&database)
            .await?;
        assert_eq!(before.0, ResourcePoolEntryState::Free, "{scenario}");
        assert_eq!(allocation.value(), None, "{scenario}");
        assert_eq!(allocation.route_target_policy(), None, "{scenario}");
        let mut txn = database.begin().await?;
        assert_eq!(allocation.allocate(&mut txn).await?, expected, "{scenario}");
        let allocated: Json<ResourcePoolEntryState> = sqlx::query_scalar(state_query)
            .bind(descriptor.name())
            .bind(expected.to_string())
            .fetch_one(&mut *txn)
            .await?;
        assert_eq!(
            allocated.0,
            ResourcePoolEntryState::Allocated {
                owner: "segment-1".to_owned(),
                owner_type: "network_segment".to_owned(),
            },
            "{scenario}",
        );

        // A failed entity write must not leave the returned value reserved, and
        // Integrated cancellation must not open a cleanup transaction either.
        txn.rollback().await?;
        allocation.cancel(&closed).await?;
        let after: Json<ResourcePoolEntryState> = sqlx::query_scalar(state_query)
            .bind(descriptor.name())
            .bind(expected.to_string())
            .fetch_one(&database)
            .await?;
        assert_eq!(after.0, ResourcePoolEntryState::Free, "{scenario}");
    }
    Ok(())
}
