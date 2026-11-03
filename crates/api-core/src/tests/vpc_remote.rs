// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! VPC creation and deletion against remote VNI pools, injected below the
//! production selection gate.

use std::sync::atomic::Ordering;
use std::time::Duration;

use carbide_resource_pool::PoolDefinitionSource;
use carbide_resource_pool_api::v1;
use carbide_uuid::vpc::VpcId;
use db::resource_pool::remote;
use model::resource_pool::OwnerType;
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolAllocation, ResourcePoolValue,
};
use model::resource_pool::common::{EXTERNAL_VPC_VNI, VPC_VNI};
use model::resource_pool::remote::{
    AllocationResult, AllocationSelection, CompletionAuthority, ProviderRouteTargetMode,
};
use rpc::forge;
use rpc::forge::forge_server::Forge;
use sqlx::PgPool;
use tonic::{Code, Request};

use crate::tests::common::resource_pool::{RemotePool, RemotePoolFixture};
use crate::tests::common::rpc_builder::{VpcCreationRequest, VpcDeletionRequest};

/// `VPC_POOLS` moves both VNI pools to the provider: internal VPCs draw
/// automatic values, while external VPCs request manual ones.
const VPC_POOLS: [RemotePool; 2] = [
    RemotePool {
        name: VPC_VNI,
        start: 20_001,
        auto_assign: true,
    },
    RemotePool {
        name: EXTERNAL_VPC_VNI,
        start: 50_001,
        auto_assign: false,
    },
];

/// `vpc_fixture` registers both VNI pools with the provider, which attaches
/// `route_target_policy` to every assignment it returns.
async fn vpc_fixture(
    database: PgPool,
    route_target_policy: Option<v1::RouteTargetPolicy>,
) -> eyre::Result<RemotePoolFixture> {
    let mut fixture =
        RemotePoolFixture::start_with(database, &VPC_POOLS, route_target_policy).await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    Ok(fixture)
}

/// `receipt` loads the VPC's latest VNI request, including the cancelled and
/// released records that active allocation listings omit.
async fn receipt(
    fixture: &RemotePoolFixture,
    vpc_id: VpcId,
) -> eyre::Result<ResourcePoolAllocation> {
    Ok(remote::latest_for_owner_slot(
        &fixture.env.pool,
        OwnerType::Vpc,
        &vpc_id.to_string(),
        "vni",
    )
    .await?
    .expect("VPC allocation request"))
}

/// `create_tenant` adds a tenant whose routing profile picks the VPC's pool.
async fn create_tenant(fixture: &RemotePoolFixture, name: &str, profile: &str) -> eyre::Result<()> {
    fixture
        .env
        .api
        .create_tenant(Request::new(forge::CreateTenantRequest {
            organization_id: name.into(),
            routing_profile_type: Some(profile.into()),
            metadata: Some(rpc::Metadata {
                name: name.into(),
                ..Default::default()
            }),
        }))
        .await?;
    Ok(())
}

/// Both VNI pools go through the API's shared create, lookup, and delete path.
/// Deletion saves the release request with the VPC delete, so a restarted API
/// can still release the provider's value. The internal case also checks that a
/// failed write of the release request undoes the deletion.
#[crate::sqlx_test]
async fn remote_vpc_creation_and_deletion_preserve_allocation_identity(
    database: PgPool,
) -> eyre::Result<()> {
    struct Case {
        profile: &'static str,
        pool: &'static str,
        requested: Option<u32>,
        expected_vni: u32,
        mode: AssignmentMode,
        check_release_failure: bool,
    }

    let mut fixture = vpc_fixture(database, None).await?;
    for case in [
        // Internal VPCs generate their ID and take an automatic value.
        Case {
            profile: "INTERNAL",
            pool: VPC_VNI,
            requested: None,
            expected_vni: 20_001,
            mode: AssignmentMode::Automatic,
            check_release_failure: true,
        },
        // External VPCs keep the requested ID and manual value.
        Case {
            profile: "EXTERNAL",
            pool: EXTERNAL_VPC_VNI,
            requested: Some(50_001),
            expected_vni: 50_001,
            mode: AssignmentMode::Manual,
            check_release_failure: false,
        },
    ] {
        create_tenant(&fixture, case.profile, case.profile).await?;
        let mut request = VpcCreationRequest::builder(case.profile)
            .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
            .routing_profile_type(case.profile);
        let requested_id = case.requested.map(|_| VpcId::new());
        if let Some(id) = requested_id {
            request = request.id(id).metadata(rpc::Metadata {
                name: case.profile.into(),
                ..Default::default()
            });
        }
        if let Some(vni) = case.requested {
            request = request.vni(vni);
        }

        let created = fixture
            .env
            .api
            .create_vpc(request.tonic_request())
            .await?
            .into_inner();

        let vpc_id = created.id.expect("created VPC ID");
        if let Some(id) = requested_id {
            assert_eq!(vpc_id, id);
        }
        assert_eq!(
            created.config.as_ref().expect("VPC config").vni,
            case.requested
        );
        assert_eq!(
            created.status.as_ref().expect("VPC status").vni,
            Some(case.expected_vni)
        );
        // Routing state finds the remote value through the VPC's ownership lookup.
        let routing = fixture
            .env
            .api
            .get_vpc_routing_state(Request::new(forge::VpcRoutingStateRequest {
                id: Some(vpc_id),
            }))
            .await?
            .into_inner();
        assert_eq!(routing.active_vni, case.expected_vni);
        assert_eq!(routing.retained_allocation, None);

        let attached = receipt(&fixture, vpc_id).await?;
        assert_eq!(attached.state, AllocationState::Attached);
        assert_eq!(attached.pool_name, case.pool);
        let AllocationSource::Remote {
            request,
            result: Some(AllocationResult::Allocated(assignment)),
            ..
        } = &attached.source
        else {
            panic!("VPC must have an assigned remote receipt: {attached:?}");
        };
        // The waiting API call owns attachment, so an abandoned request expires.
        assert!(matches!(
            request.completion_authority,
            CompletionAuthority::Foreground { .. }
        ));
        assert_eq!(
            request.selection,
            case.requested
                .map_or(AllocationSelection::Automatic, |vni| {
                    AllocationSelection::Requested(ResourcePoolValue::Integer(i64::from(vni)))
                })
        );
        assert_eq!(
            assignment.value,
            ResourcePoolValue::Integer(i64::from(case.expected_vni))
        );
        assert_eq!(assignment.assignment_mode, case.mode);
        assert!(
            fixture
                .provider
                .local
                .is_provisioned(&assignment.allocation_id)
        );

        if case.check_release_failure {
            // A failed write of the release request must undo the VPC deletion.
            sqlx::query(
                "ALTER TABLE resource_pool_allocations ADD CONSTRAINT reject_test_release
                    CHECK (state <> 'release_requested')",
            )
            .execute(&fixture.env.pool)
            .await?;
            let error = fixture
                .env
                .api
                .delete_vpc(VpcDeletionRequest::builder().id(vpc_id).tonic_request())
                .await
                .expect_err("injected release request write failure");
            assert_eq!(error.code(), Code::Internal);
            assert_eq!(error.message(), "internal database error");
            let found = fixture
                .env
                .api
                .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest {
                    vpc_ids: vec![vpc_id],
                }))
                .await?
                .into_inner();
            assert_eq!(found.vpcs.as_slice(), std::slice::from_ref(&created));
            assert_eq!(receipt(&fixture, vpc_id).await?, attached);
            sqlx::query(
                "ALTER TABLE resource_pool_allocations DROP CONSTRAINT reject_test_release",
            )
            .execute(&fixture.env.pool)
            .await?;
        }

        // Deletion commits release intent; the provider isn't contacted yet.
        let releases = fixture.provider.releases.load(Ordering::SeqCst);
        fixture
            .env
            .api
            .delete_vpc(VpcDeletionRequest::builder().id(vpc_id).tonic_request())
            .await?;
        let deleted = fixture
            .env
            .api
            .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest {
                vpc_ids: vec![vpc_id],
            }))
            .await?
            .into_inner();
        assert!(deleted.vpcs.is_empty());
        let releasing = receipt(&fixture, vpc_id).await?;
        assert_eq!(releasing.state, AllocationState::ReleaseRequested);
        assert_eq!(releasing.source, attached.source);
        assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), releases);

        // A restarted API finishes the release from the saved request alone.
        fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
        fixture
            .remote(case.pool)
            .reconcile(attached.allocation_id, &fixture.cancel)
            .await?;
        assert_eq!(
            receipt(&fixture, vpc_id).await?.state,
            AllocationState::Released
        );
        assert!(
            !fixture
                .provider
                .local
                .is_provisioned(&assignment.allocation_id)
        );
        assert_eq!(
            fixture.provider.releases.load(Ordering::SeqCst),
            releases + 1
        );
    }
    fixture.stop().await
}

/// A failed VPC insert can't roll back provider work, so creation saves
/// cancellation for recovery. Once that finishes, retrying with the same VPC ID
/// starts a new allocation instead of reopening the cancelled one.
#[crate::sqlx_test]
async fn remote_vpc_insert_failure_recovers_cancellation(database: PgPool) -> eyre::Result<()> {
    let mut fixture = vpc_fixture(database, None).await?;
    sqlx::query("ALTER TABLE vpcs ADD CONSTRAINT reject_remote_vpc CHECK (name <> 'failed-api')")
        .execute(&fixture.env.pool)
        .await?;
    let vpc_id = VpcId::new();
    let request = VpcCreationRequest::builder("failed-api-tenant")
        .id(vpc_id)
        .metadata(rpc::Metadata {
            name: "failed-api".into(),
            ..Default::default()
        })
        .network_virtualization_type(forge::VpcVirtualizationType::Flat as i32)
        .tonic_request()
        .into_inner();

    let error = fixture
        .env
        .api
        .create_vpc(Request::new(request.clone()))
        .await
        .expect_err("injected VPC insert failure");
    assert_eq!(error.code(), Code::Internal);
    assert_eq!(error.message(), "internal database error");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM vpcs WHERE id = $1")
        .bind(vpc_id)
        .fetch_one(&fixture.env.pool)
        .await?;
    assert_eq!(count, 0);
    let cancelling = receipt(&fixture, vpc_id).await?;
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(first_assignment)),
        ..
    } = &cancelling.source
    else {
        panic!("failed creation must keep its provider assignment: {cancelling:?}");
    };
    assert_eq!(fixture.provider.local.provisioned_count(), 1);

    // A restarted API cancels the provider reservation from the saved request.
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    fixture
        .remote(VPC_VNI)
        .reconcile(cancelling.allocation_id, &fixture.cancel)
        .await?;
    let cancelled = receipt(&fixture, vpc_id).await?;
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.source, cancelling.source);
    assert_eq!(fixture.provider.local.provisioned_count(), 0);

    sqlx::query("ALTER TABLE vpcs DROP CONSTRAINT reject_remote_vpc")
        .execute(&fixture.env.pool)
        .await?;
    let created = fixture
        .env
        .api
        .create_vpc(Request::new(request))
        .await?
        .into_inner();
    assert_eq!(created.id, Some(vpc_id));
    let attached = receipt(&fixture, vpc_id).await?;
    assert_eq!(attached.state, AllocationState::Attached);
    assert_ne!(attached.allocation_id, cancelled.allocation_id);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(second_assignment)),
        ..
    } = &attached.source
    else {
        panic!("retry must attach a new allocation: {attached:?}");
    };
    assert_ne!(
        second_assignment.allocation_id,
        first_assignment.allocation_id
    );
    assert_eq!(
        remote::load(&fixture.env.pool, cancelled.allocation_id).await?,
        Some(cancelled)
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}

/// Creation releases the tenant's row lock while it waits for the provider.
/// A routing change during that wait, or an API shutdown, must cancel the
/// request instead of attaching a VNI from the wrong pool or abandoning it.
#[crate::sqlx_test]
async fn remote_vpc_cancels_after_admission_changes_or_shutdown(
    database: PgPool,
) -> eyre::Result<()> {
    #[derive(Clone, Copy)]
    enum Interruption {
        TenantRoutingChanged,
        ApiStopped,
    }

    let fixture = vpc_fixture(database, None).await?;
    // Shutdown is last, because it stops this fixture's API.
    for (tenant, interruption) in [
        ("changing-tenant", Interruption::TenantRoutingChanged),
        ("stopping-tenant", Interruption::ApiStopped),
    ] {
        create_tenant(&fixture, tenant, "INTERNAL").await?;
        let vpc_id = VpcId::new();
        let request = VpcCreationRequest::builder(tenant)
            .id(vpc_id)
            .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
            .tonic_request()
            .into_inner();
        let allocations = fixture.provider.allocations.load(Ordering::SeqCst);
        let pause = fixture.provider.allocate_pause.clone();
        pause.pause_next.store(true, Ordering::SeqCst);

        // Hold the provider's reply after it has reserved a value.
        let creation = fixture.env.api.create_vpc(Request::new(request.clone()));
        tokio::pin!(creation);
        tokio::select! {
            result = &mut creation => {
                panic!("creation finished before its allocation reply: {result:?}");
            }
            reached = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => {
                reached?;
            }
        }
        assert_eq!(fixture.provider.local.provisioned_count(), 1);

        match interruption {
            Interruption::TenantRoutingChanged => {
                // The tenant update locks the tenant row, so it would block if
                // creation still held that lock while waiting for the provider.
                tokio::time::timeout(
                    Duration::from_secs(5),
                    fixture
                        .env
                        .api
                        .update_tenant(Request::new(forge::UpdateTenantRequest {
                            organization_id: tenant.into(),
                            metadata: Some(rpc::Metadata {
                                name: tenant.into(),
                                ..Default::default()
                            }),
                            routing_profile_type: Some("EXTERNAL".into()),
                            ..Default::default()
                        })),
                )
                .await??;
                pause.resume.notify_one();
                let error = tokio::time::timeout(Duration::from_secs(5), creation)
                    .await?
                    .expect_err("tenant changed allocation pools");
                assert_eq!(error.code(), Code::FailedPrecondition);
                assert!(error.message().contains("routing changed"), "{error}");
            }
            Interruption::ApiStopped => {
                // A retry with the same VPC ID can't adopt the waiting request.
                let pending = receipt(&fixture, vpc_id).await?;
                assert_eq!(pending.state, AllocationState::Pending);
                let error = tokio::time::timeout(
                    Duration::from_secs(5),
                    fixture.env.api.create_vpc(Request::new(request)),
                )
                .await?
                .expect_err("another call cannot adopt the waiting request");
                assert_eq!(error.code(), Code::FailedPrecondition);
                assert!(
                    error
                        .message()
                        .contains("unfinished request for slot `vni`"),
                    "{error}"
                );
                assert_eq!(receipt(&fixture, vpc_id).await?, pending);

                // Shutdown stops the provider wait and saves cleanup first.
                fixture.env.api.cancel_token.cancel();
                let error = tokio::time::timeout(Duration::from_secs(5), creation)
                    .await?
                    .expect_err("API stopped during allocation");
                assert_eq!(error.code(), Code::Cancelled);
                // Core abandoned the provider call rather than leaving it held.
                tokio::time::timeout(Duration::from_secs(5), pause.dropped.cancelled()).await?;
            }
        }

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM vpcs WHERE id = $1")
            .bind(vpc_id)
            .fetch_one(&fixture.env.pool)
            .await?;
        assert_eq!(count, 0);
        let cancelling = receipt(&fixture, vpc_id).await?;
        assert_eq!(cancelling.state, AllocationState::CancelRequested);
        // Cancellation keeps exactly the reply state Core saw, if any.
        match (interruption, &cancelling.source) {
            (
                Interruption::TenantRoutingChanged,
                AllocationSource::Remote {
                    result: Some(AllocationResult::Allocated(_)),
                    ..
                },
            )
            | (Interruption::ApiStopped, AllocationSource::Remote { result: None, .. }) => {}
            _ => panic!("cancellation must keep the reply state Core saw: {cancelling:?}"),
        }

        // Recovery uses the fixture's token, so cleanup still runs after shutdown.
        fixture
            .remote(VPC_VNI)
            .reconcile(cancelling.allocation_id, &fixture.cancel)
            .await?;
        let cancelled = receipt(&fixture, vpc_id).await?;
        assert_eq!(cancelled.state, AllocationState::Cancelled);
        assert_eq!(cancelled.source, cancelling.source);
        assert_eq!(fixture.provider.local.provisioned_count(), 0);
        assert_eq!(
            fixture.provider.allocations.load(Ordering::SeqCst),
            allocations + 1
        );
    }
    fixture.stop().await
}

/// VPCs don't store provider route-target policy yet. Creation rejects an
/// assignment that includes one before attaching it, and keeps the request so
/// recovery can return the value.
#[crate::sqlx_test]
async fn remote_vpc_cancels_an_unsupported_provider_policy(database: PgPool) -> eyre::Result<()> {
    let fixture = vpc_fixture(
        database,
        Some(v1::RouteTargetPolicy {
            mode: v1::RouteTargetMode::PreserveGenerated as i32,
            ..Default::default()
        }),
    )
    .await?;
    let vpc_id = VpcId::new();

    let error = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("policy-tenant")
                .id(vpc_id)
                .metadata(rpc::Metadata {
                    name: "unsupported-provider-policy".into(),
                    ..Default::default()
                })
                .network_virtualization_type(forge::VpcVirtualizationType::Flat as i32)
                .tonic_request(),
        )
        .await
        .expect_err("VPCs cannot apply provider routing policy");

    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("provider route-target policy is not supported"),
        "{error}"
    );
    assert!(
        db::vpc::find_by_name(&fixture.env.pool, "unsupported-provider-policy")
            .await?
            .is_empty()
    );
    let cancelling = receipt(&fixture, vpc_id).await?;
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &cancelling.source
    else {
        panic!("rejected policy must keep the provider assignment: {cancelling:?}");
    };
    assert_eq!(
        assignment
            .route_target_policy
            .as_ref()
            .expect("saved provider policy")
            .mode,
        ProviderRouteTargetMode::Preserve,
    );

    fixture
        .remote(VPC_VNI)
        .reconcile(cancelling.allocation_id, &fixture.cancel)
        .await?;
    assert_eq!(
        receipt(&fixture, vpc_id).await?.state,
        AllocationState::Cancelled
    );
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    fixture.stop().await
}
