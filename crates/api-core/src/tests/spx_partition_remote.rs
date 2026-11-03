// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! SPX partition creation and deletion against a remote `dpa-vni` pool,
//! injected below the production selection gate.

use std::sync::atomic::Ordering;

use carbide_resource_pool::PoolDefinitionSource;
use carbide_uuid::spx::SpxPartitionId;
use db::resource_pool::remote;
use model::resource_pool::OwnerType;
use model::resource_pool::allocation::{AllocationSource, AllocationState, ResourcePoolAllocation};
use model::resource_pool::common::DPA_VNI;
use model::resource_pool::remote::AllocationResult;
use rpc::forge;
use rpc::forge::forge_server::Forge;
use sqlx::PgPool;
use tonic::{Code, Request, Response, Status};

use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::resource_pool::{RemotePool, RemotePoolFixture};

/// `dpa_vni_fixture` moves `dpa-vni` to a provider that holds only `value`.
/// The pool's definition starts at `value`, so the provider assigns it to the
/// first partition.
async fn dpa_vni_fixture(database: PgPool, value: i64) -> eyre::Result<RemotePoolFixture> {
    let pools = [RemotePool {
        name: DPA_VNI,
        start: value,
        auto_assign: true,
    }];
    let mut fixture =
        RemotePoolFixture::start_with_values(database, &pools, &[value], None).await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    Ok(fixture)
}

/// `create_partition` creates partition `id` with an automatic VNI.
async fn create_partition(
    fixture: &RemotePoolFixture,
    id: SpxPartitionId,
    name: &str,
) -> Result<forge::SpxPartition, Status> {
    fixture
        .env
        .api
        .create_spx_partition(Request::new(forge::SpxPartitionCreationRequest {
            id: Some(id),
            metadata: Some(forge::Metadata {
                name: name.into(),
                ..Default::default()
            }),
            tenant_organization_id: FIXTURE_TENANT_ORG_ID.into(),
            ..Default::default()
        }))
        .await
        .map(Response::into_inner)
}

/// `delete_partition` deletes partition `id` through the API.
async fn delete_partition(fixture: &RemotePoolFixture, id: SpxPartitionId) -> eyre::Result<()> {
    fixture
        .env
        .api
        .delete_spx_partition(Request::new(forge::SpxPartitionDeletionRequest {
            id: Some(id),
        }))
        .await?;
    Ok(())
}

/// `receipt` loads the partition's latest VNI request, including the cancelled
/// and released records that active allocation listings omit.
async fn receipt(
    fixture: &RemotePoolFixture,
    id: SpxPartitionId,
) -> eyre::Result<ResourcePoolAllocation> {
    Ok(remote::latest_for_owner_slot(
        &fixture.env.pool,
        OwnerType::SpxPartition,
        &id.to_string(),
        "vni",
    )
    .await?
    .expect("SPX partition allocation request"))
}

/// `partition_rows` counts the partition's rows, including a deleted one.
async fn partition_rows(fixture: &RemotePoolFixture, id: SpxPartitionId) -> eyre::Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM spx_partitions WHERE id = $1")
            .bind(id)
            .fetch_one(&fixture.env.pool)
            .await?,
    )
}

/// Creation attaches the provider's value to the partition, and deletion saves a
/// release request in the same transaction that marks the partition deleted.
/// Repeating the deletion finds no VNI the partition still owns, so the request
/// stays unchanged.
#[crate::sqlx_test]
async fn remote_spx_partition_attaches_then_requests_release_once(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = dpa_vni_fixture(database, 20_001).await?;
    let id = SpxPartitionId::new();

    let created = create_partition(&fixture, id, "remote partition").await?;
    assert_eq!(created.id, Some(id));
    assert_eq!(created.vni, 20_001);
    let attached = receipt(&fixture, id).await?;
    assert_eq!(attached.state, AllocationState::Attached);
    assert_eq!(attached.pool_name, DPA_VNI);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &attached.source
    else {
        panic!("partition must have an assigned remote receipt: {attached:?}");
    };
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );

    // Deletion commits release intent; the provider isn't contacted yet.
    let releases = fixture.provider.releases.load(Ordering::SeqCst);
    delete_partition(&fixture, id).await?;
    let is_deleted: bool =
        sqlx::query_scalar("SELECT deleted IS NOT NULL FROM spx_partitions WHERE id = $1")
            .bind(id)
            .fetch_one(&fixture.env.pool)
            .await?;
    assert!(is_deleted);
    let releasing = receipt(&fixture, id).await?;
    assert_eq!(releasing.state, AllocationState::ReleaseRequested);
    assert_eq!(releasing.source, attached.source);
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), releases);

    delete_partition(&fixture, id).await?;
    assert_eq!(receipt(&fixture, id).await?, releasing);
    fixture.stop().await
}

/// A failed partition insert can't roll back the provider's reservation, so
/// creation saves a cancellation request that keeps the provider's assignment.
#[crate::sqlx_test]
async fn remote_spx_partition_insert_failure_cancels_its_request(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = dpa_vni_fixture(database, 20_001).await?;
    sqlx::query(
        "ALTER TABLE spx_partitions ADD CONSTRAINT reject_remote_partition
            CHECK (name <> 'failed-partition')",
    )
    .execute(&fixture.env.pool)
    .await?;
    let id = SpxPartitionId::new();

    let error = create_partition(&fixture, id, "failed-partition")
        .await
        .expect_err("injected partition insert failure");
    assert_eq!(error.code(), Code::Internal);
    assert_eq!(error.message(), "internal database error");
    assert_eq!(partition_rows(&fixture, id).await?, 0);
    let cancelling = receipt(&fixture, id).await?;
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    assert!(
        matches!(
            &cancelling.source,
            AllocationSource::Remote {
                result: Some(AllocationResult::Allocated(_)),
                ..
            }
        ),
        "failed creation must keep its provider assignment: {cancelling:?}"
    );
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}

/// A pool definition can admit a value that SPX partitions can't use. Creation
/// rejects a provider VNI wider than 24 bits before attaching it, and saves a
/// cancellation request for it.
#[crate::sqlx_test]
async fn remote_spx_partition_rejects_an_unusable_provider_vni(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = dpa_vni_fixture(database, 16_777_216).await?;
    let id = SpxPartitionId::new();

    let error = create_partition(&fixture, id, "unusable VNI")
        .await
        .expect_err("a VNI wider than 24 bits");
    assert_eq!(error.code(), Code::Internal, "{error:?}");
    assert_eq!(
        error.message(),
        "internal error: VNI provider returned `16777216`; expected a value between 1 and 16777215"
    );
    assert_eq!(partition_rows(&fixture, id).await?, 0);
    assert_eq!(
        receipt(&fixture, id).await?.state,
        AllocationState::CancelRequested
    );
    fixture.stop().await
}
