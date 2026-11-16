// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! VPC creation, routing, retained release and recovery against remote VNI pools.
//! Providers are injected below the production selection gate.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use carbide_instrument::testing::{MetricsCapture, capture_logs_async};
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_resource_pool::PoolDefinitionSource;
use carbide_resource_pool_api::v1;
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use carbide_uuid::vpc::VpcId;
use db::resource_pool::remote;
use db::test_support::postgres::wait_for_blocked_queries;
use model::instance::config::network::{InstanceInterfaceRoutingProfile, InstanceNetworkConfig};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::resource_pool::OwnerType;
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolAllocation, ResourcePoolValue,
};
use model::resource_pool::common::{EXTERNAL_VPC_VNI, VPC_VNI};
use model::resource_pool::remote::{
    AllocationResult, AllocationSelection, CompletionAuthority, ProviderRouteTargetMode,
};
use model::vpc::VpcDefinition;
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

/// A later seed failure preserves earlier commits. Recovery cleans only the
/// failed attempt even when the next startup removes or changes its definition.
#[crate::sqlx_test]
async fn configured_vpcs_resume_after_partial_creation(database: PgPool) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start_with(database, &VPC_POOLS, None).await?;
    // Leave a third value eligible so the replacement can attach before the
    // original request has finished cancellation.
    fixture
        .definitions
        .get_mut(VPC_VNI)
        .expect("VNI definition")
        .ranges[0]
        .end = (VPC_POOLS[0].start + 3).to_string();
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let definition = VpcDefinition {
        organization_id: Some("initial-tenant".into()),
        network_virtualization_type: VpcVirtualizationType::Flat,
        routing_profile_type: None,
        routing_profile_overrides: None,
        vni: None,
    };
    let mut definitions = HashMap::from([
        ("a-created".into(), definition.clone()),
        ("z-failed".into(), definition),
    ]);
    sqlx::query("ALTER TABLE vpcs ADD CONSTRAINT reject_initial_vpc CHECK (name <> 'z-failed')")
        .execute(&fixture.env.pool)
        .await?;
    let error = crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions)
        .await
        .expect_err("the second configured VPC cannot be inserted");
    assert!(error.to_string().contains("reject_initial_vpc"), "{error}");
    let created = db::vpc::find_by_name(&fixture.env.pool, "a-created").await?;
    assert_eq!(created.len(), 1);
    assert_eq!(
        receipt(&fixture, created[0].id).await?.state,
        AllocationState::Attached
    );
    assert!(
        db::vpc::find_by_name(&fixture.env.pool, "z-failed")
            .await?
            .is_empty()
    );
    let cancelled_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT allocation_id FROM resource_pool_allocations WHERE state = 'cancel_requested'",
    )
    .fetch_all(&fixture.env.pool)
    .await?;
    assert_eq!(cancelled_ids.len(), 1);
    let cancelled_id = cancelled_ids[0];
    let cancelled = remote::load(&fixture.env.pool, cancelled_id)
        .await?
        .expect("saved cancellation");
    assert_eq!(fixture.provider.local.provisioned_count(), 2);

    // Removing the failed entry does not dispatch another attempt or touch the
    // already committed VPC. The old request remains available for recovery.
    let mut changed = definitions.remove("z-failed").expect("failed definition");
    let allocations = fixture.provider.allocations.load(Ordering::SeqCst);
    assert_eq!(allocations, 2);
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions).await?;
    assert_eq!(
        fixture.provider.allocations.load(Ordering::SeqCst),
        allocations
    );
    assert_eq!(
        db::vpc::find_by_name(&fixture.env.pool, "a-created").await?,
        created
    );
    // The replacement definition gets its own owner and survives cleanup of
    // the original request, which must never cancel a later allocation.
    sqlx::query("ALTER TABLE vpcs DROP CONSTRAINT reject_initial_vpc")
        .execute(&fixture.env.pool)
        .await?;
    changed.organization_id = Some("replacement-tenant".into());
    definitions.insert("z-failed".into(), changed);
    crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions).await?;
    let replacement = db::vpc::find_by_name(&fixture.env.pool, "z-failed").await?;
    assert_eq!(replacement.len(), 1);
    assert_eq!(
        replacement[0].config.tenant_organization_id,
        "replacement-tenant"
    );
    let attached = receipt(&fixture, replacement[0].id).await?;
    assert_eq!(attached.state, AllocationState::Attached);
    assert_ne!(attached.allocation_id, cancelled.allocation_id);
    assert_ne!(attached.owner_id, cancelled.owner_id);
    assert_eq!(fixture.provider.local.provisioned_count(), 3);
    fixture
        .remote(VPC_VNI)
        .reconcile(cancelled_id, &fixture.cancel)
        .await?;
    assert_eq!(receipt(&fixture, replacement[0].id).await?, attached);
    assert_eq!(
        remote::load(&fixture.env.pool, cancelled_id)
            .await?
            .expect("old request")
            .state,
        AllocationState::Cancelled,
    );
    assert_eq!(fixture.provider.local.provisioned_count(), 2);
    fixture.stop().await
}

/// Two startups can reserve independently, but serialize their final name
/// check. The loser cancels its reservation without changing the winning VPC.
#[crate::sqlx_test]
async fn configured_vpcs_converge_on_one_name(database: PgPool) -> eyre::Result<()> {
    let fixture = vpc_fixture(database, None).await?;
    let definitions = HashMap::from([(
        "concurrent-initial".into(),
        VpcDefinition {
            organization_id: None,
            network_virtualization_type: VpcVirtualizationType::Fnn,
            routing_profile_type: Some("EXTERNAL".into()),
            routing_profile_overrides: None,
            vni: None,
        },
    )]);
    let mut blocker = fixture.env.pool.begin().await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    db::vpc::lock_initial_name(&mut blocker, "concurrent-initial").await?;
    let (first, second) = {
        let creation = async {
            tokio::join!(
                crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions),
                crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions),
            )
        };
        tokio::pin!(creation);
        tokio::select! {
            result = &mut creation => panic!("creation bypassed the final name lock: {result:?}"),
            _ = wait_for_blocked_queries(&fixture.env.pool, blocker_pid, "initial-vpc:", 2) => {}
        }
        assert_eq!(fixture.provider.local.provisioned_count(), 2);
        blocker.commit().await?;
        tokio::time::timeout(Duration::from_secs(10), creation).await?
    };
    first?;
    second?;
    let created = db::vpc::find_by_name(&fixture.env.pool, "concurrent-initial").await?;
    assert_eq!(created.len(), 1);
    let created = &created[0];
    uuid::Uuid::parse_str(&created.config.tenant_organization_id)?;
    assert_eq!(
        created.config.routing_profile_type.as_deref(),
        Some("EXTERNAL")
    );
    let attached = receipt(&fixture, created.id).await?;
    assert_eq!(attached.pool_name, VPC_VNI);
    assert_eq!(attached.state, AllocationState::Attached);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &attached.source
    else {
        panic!("attached provider assignment: {attached:?}");
    };
    let ResourcePoolValue::Integer(vni) = assignment.value else {
        panic!("integer VNI: {assignment:?}");
    };
    assert_eq!(created.config.vni, Some(i32::try_from(vni)?));
    assert_eq!(created.status.vni, Some(i32::try_from(vni)?));
    let cancelled_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT allocation_id FROM resource_pool_allocations WHERE state = 'cancel_requested'",
    )
    .fetch_all(&fixture.env.pool)
    .await?;
    assert_eq!(cancelled_ids.len(), 1);
    let cancelled_id = cancelled_ids[0];
    fixture
        .remote(VPC_VNI)
        .reconcile(cancelled_id, &fixture.cancel)
        .await?;
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    assert_eq!(receipt(&fixture, created.id).await?, attached);
    crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions).await?;
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    fixture.stop().await
}

/// A configured manual VNI must reach the provider's requested partition and
/// be saved in both fields, just as configured Integrated allocations are.
#[crate::sqlx_test]
async fn configured_vpc_preserves_a_requested_vni(database: PgPool) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start_with(
        database,
        &[RemotePool {
            name: VPC_VNI,
            start: 20_001,
            auto_assign: false,
        }],
        None,
    )
    .await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let definitions = HashMap::from([(
        "requested-initial".into(),
        VpcDefinition {
            organization_id: Some("initial-tenant".into()),
            network_virtualization_type: VpcVirtualizationType::Flat,
            routing_profile_type: None,
            routing_profile_overrides: None,
            vni: Some(20_001),
        },
    )]);
    crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions).await?;
    let created = db::vpc::find_by_name(&fixture.env.pool, "requested-initial").await?;
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].config.vni, Some(20_001));
    assert_eq!(created[0].status.vni, Some(20_001));
    let attached = receipt(&fixture, created[0].id).await?;
    assert_eq!(attached.state, AllocationState::Attached);
    let AllocationSource::Remote {
        request,
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &attached.source
    else {
        panic!("requested provider assignment: {attached:?}");
    };
    assert_eq!(
        request.selection,
        AllocationSelection::Requested(ResourcePoolValue::Integer(20_001))
    );
    assert_eq!(assignment.value, ResourcePoolValue::Integer(20_001));
    assert_eq!(assignment.assignment_mode, AssignmentMode::Manual);
    fixture.stop().await
}

/// Lookup errors must remain SELECT failures. Before preparation they must
/// prevent dispatch; after preparation they must leave the assignment for cleanup.
#[crate::sqlx_test]
async fn configured_vpc_lookup_failures_preserve_cleanup(database: PgPool) -> eyre::Result<()> {
    let fixture = vpc_fixture(database, None).await?;
    let definitions = HashMap::from([(
        "lookup-failure".into(),
        VpcDefinition {
            organization_id: Some("initial-tenant".into()),
            network_virtualization_type: VpcVirtualizationType::Flat,
            routing_profile_type: None,
            routing_profile_overrides: None,
            vni: None,
        },
    )]);
    for after_preparation in [false, true] {
        let error = if after_preparation {
            // Let the initial lookup and provider reservation succeed before
            // making the final, locked name lookup fail.
            let pause = fixture.provider.allocate_pause.clone();
            pause.pause_next.store(true, Ordering::SeqCst);
            let creation = crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions);
            tokio::pin!(creation);
            tokio::select! {
                result = &mut creation => panic!("creation bypassed the reply gate: {result:?}"),
                reached = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => reached?,
            }
            assert_eq!(fixture.provider.local.provisioned_count(), 1);
            sqlx::query("ALTER TABLE vpcs RENAME TO unavailable_vpcs").execute(&fixture.env.pool).await?;
            pause.resume.notify_one();
            tokio::time::timeout(Duration::from_secs(5), creation).await?
        } else {
            // The first lookup fails while the provider is otherwise usable.
            sqlx::query("ALTER TABLE vpcs RENAME TO unavailable_vpcs").execute(&fixture.env.pool).await?;
            crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions).await
        }.expect_err("name lookup must fail");
        let crate::CarbideError::DBError(annotated) = &error else {
            panic!("expected the original lookup error: {error:?}");
        };
        assert_eq!(
            annotated
                .source
                .as_database_error()
                .and_then(|e| e.code())
                .as_deref(),
            Some("42P01")
        );
        let diagnostic = format!("{annotated:?}");
        assert!(diagnostic.contains("SELECT"), "{diagnostic}");
        assert!(!diagnostic.contains("INSERT"), "{diagnostic}");
        sqlx::query("ALTER TABLE unavailable_vpcs RENAME TO vpcs")
            .execute(&fixture.env.pool)
            .await?;
        assert!(
            db::vpc::find_by_name(&fixture.env.pool, "lookup-failure")
                .await?
                .is_empty()
        );
        assert_eq!(
            fixture.provider.allocations.load(Ordering::SeqCst),
            usize::from(after_preparation)
        );
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT allocation_id FROM resource_pool_allocations WHERE state = 'cancel_requested'",
        )
        .fetch_all(&fixture.env.pool)
        .await?;
        assert_eq!(ids.len(), usize::from(after_preparation));
        if let Some(id) = ids.first() {
            fixture
                .remote(VPC_VNI)
                .reconcile(*id, &fixture.cancel)
                .await?;
            assert_eq!(
                remote::load(&fixture.env.pool, *id)
                    .await?
                    .expect("failed request")
                    .state,
                AllocationState::Cancelled
            );
        }
        assert_eq!(fixture.provider.local.provisioned_count(), 0);
    }
    fixture.stop().await
}

/// A stalled startup must not hold another startup indefinitely. Both the
/// server deadline and shutdown interrupt its lock wait before saving cleanup.
#[crate::sqlx_test]
async fn configured_vpc_name_wait_is_bounded_and_cancellable(database: PgPool) -> eyre::Result<()> {
    let fixture = vpc_fixture(database, None).await?;
    // With no operator limit, only the configured completion transaction can
    // stop the SQL wait. This proof intentionally waits for its five-second bound.
    let timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&fixture.env.pool)
        .await?;
    assert_eq!(timeout, "0");
    for shutdown in [false, true] {
        let name = if shutdown {
            "cancelled-name-wait"
        } else {
            "expired-name-wait"
        };
        let definitions = HashMap::from([(
            name.into(),
            VpcDefinition {
                organization_id: Some("initial-tenant".into()),
                network_virtualization_type: VpcVirtualizationType::Flat,
                routing_profile_type: None,
                routing_profile_overrides: None,
                vni: None,
            },
        )]);
        let mut blocker = fixture.env.pool.begin().await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        db::vpc::lock_initial_name(&mut blocker, name).await?;
        let error = {
            let creation = crate::db_init::create_initial_vpcs(&fixture.env.api, &definitions);
            tokio::pin!(creation);
            tokio::select! {
                result = &mut creation => panic!("creation bypassed the name lock: {result:?}"),
                _ = wait_for_blocked_queries(&fixture.env.pool, pid, "initial-vpc:", 1) => {},
            }
            assert_eq!(fixture.provider.local.provisioned_count(), 1);
            if shutdown {
                // Cancel only once PostgreSQL confirms the operation is blocked.
                fixture.env.api.cancel_token.cancel();
            }
            tokio::time::timeout(Duration::from_secs(8), creation)
                .await?
                .expect_err("blocked creation must stop")
        };
        if shutdown {
            assert_eq!(tonic::Status::from(error).code(), Code::Cancelled);
        } else {
            assert!(error.to_string().contains("statement timeout"), "{error:?}");
        }
        assert!(
            db::vpc::find_by_name(&fixture.env.pool, name)
                .await?
                .is_empty()
        );
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT allocation_id FROM resource_pool_allocations WHERE state = 'cancel_requested'",
        )
        .fetch_all(&fixture.env.pool)
        .await?;
        assert_eq!(ids.len(), 1);
        // Keep the blocker held through the result and cleanup-intent checks.
        blocker.commit().await?;
        fixture
            .remote(VPC_VNI)
            .reconcile(ids[0], &fixture.cancel)
            .await?;
        assert_eq!(
            remote::load(&fixture.env.pool, ids[0])
                .await?
                .expect("interrupted request")
                .state,
            AllocationState::Cancelled
        );
        assert_eq!(fixture.provider.local.provisioned_count(), 0);
    }
    fixture.stop().await
}

/// A routing switch retains both issuing identities and rejects duplicate-ID creation.
/// Switching back makes no allocation or recovery call; release waits for confirmation.
#[crate::sqlx_test]
async fn remote_routing_retains_assignments_until_confirmed_release(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start_with(database, &VPC_POOLS, None).await?;
    let mut config = (*fixture.env.config).clone();
    config.tenant_prefix_overlap_enabled = true;
    config.vpc_isolation_behavior = crate::cfg::file::VpcIsolationBehaviorType::MutualIsolation;
    config.site_fabric_prefixes = vec!["10.0.0.0/8".parse()?];
    config
        .fnn
        .as_mut()
        .expect("FNN config")
        .routing_profiles
        .get_mut("INTERNAL")
        .expect("internal profile")
        .tenant_prefix_overlap_eligible = true;
    fixture.env.config = std::sync::Arc::new(config);
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    create_tenant(&fixture, "remote-routing", "INTERNAL").await?;
    let created = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("remote-routing")
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("EXTERNAL")
                .vni(50_001_u32)
                .tonic_request(),
        )
        .await?
        .into_inner();
    let id = created.id.expect("VPC ID");
    let original = receipt(&fixture, id).await?;
    // Another pool can retain an assignment for this owner during routing.
    // Creation must reject the existing ID before using that shared capability.
    let allocations_before = fixture.provider.allocations.load(Ordering::SeqCst);
    let duplicate = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("remote-routing")
                .id(id)
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL")
                .tonic_request(),
        )
        .await
        .expect_err("an existing VPC cannot reserve in another pool");
    assert_eq!(duplicate.code(), Code::AlreadyExists);
    assert_eq!(
        fixture.provider.allocations.load(Ordering::SeqCst),
        allocations_before
    );
    assert_eq!(receipt(&fixture, id).await?, original);
    let unchanged = fixture
        .env
        .api
        .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest { vpc_ids: vec![id] }))
        .await?
        .into_inner();
    assert_eq!(unchanged.vpcs.as_slice(), std::slice::from_ref(&created));
    let change = |version: String, profile: &str, vni| forge::VpcChangeRoutingProfileRequest {
        id: Some(id),
        if_version_match: Some(version),
        routing_profile_type: profile.into(),
        vni,
    };
    // Exact selection must accept this automatic value, unlike creation's
    // requested selection, which is limited to the manual partition.
    let forward = fixture
        .env
        .api
        .change_vpc_routing_profile(Request::new(change(
            created.version,
            "INTERNAL",
            Some(20_001),
        )))
        .await?
        .into_inner();
    let internal = receipt(&fixture, id).await?;
    assert_eq!(internal.state, AllocationState::Attached);
    let AllocationSource::Remote {
        request,
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &internal.source
    else {
        panic!("internal VNI must retain its assignment");
    };
    assert_eq!(
        request.selection,
        AllocationSelection::Exact(ResourcePoolValue::Integer(20_001))
    );
    assert_eq!(assignment.assignment_mode, AssignmentMode::Automatic);
    assert_eq!(
        forward
            .retained_allocation
            .as_ref()
            .expect("retained external")
            .vni,
        50_001
    );
    assert_eq!(
        fixture.provider.allocations.load(Ordering::SeqCst),
        2,
        "creation and exact transition contacted the provider"
    );
    let recoveries = fixture.provider.recoveries.load(Ordering::SeqCst);
    let describes = fixture.provider.describes.load(Ordering::SeqCst);
    let releases = fixture.provider.releases.load(Ordering::SeqCst);
    fixture
        .provider
        .allocation_unavailable
        .store(true, Ordering::SeqCst);
    let reverse = fixture
        .env
        .api
        .change_vpc_routing_profile(Request::new(change(forward.version, "EXTERNAL", None)))
        .await?
        .into_inner();
    assert_eq!(reverse.active_vni, 50_001);
    let restored = fixture
        .env
        .api
        .change_vpc_routing_profile(Request::new(change(reverse.version, "INTERNAL", None)))
        .await?
        .into_inner();
    fixture
        .provider
        .allocation_unavailable
        .store(false, Ordering::SeqCst);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(
        fixture.provider.recoveries.load(Ordering::SeqCst),
        recoveries
    );
    assert_eq!(
        restored
            .retained_allocation
            .as_ref()
            .expect("retained external")
            .pool_name,
        EXTERNAL_VPC_VNI
    );
    assert_eq!(restored.active_vni, 20_001);
    assert_eq!(fixture.provider.describes.load(Ordering::SeqCst), describes);
    assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), releases);
    assert_eq!(receipt(&fixture, id).await?, internal);
    assert_eq!(
        remote::load(&fixture.env.pool, original.allocation_id).await?,
        Some(original.clone())
    );
    assert_eq!(fixture.provider.local.provisioned_count(), 2);

    // A second internal VPC can reuse this tenant's exact prefix only after the
    // old external assignment has been drained and its provider release confirmed.
    let candidate = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("remote-routing")
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL")
                .tonic_request(),
        )
        .await?
        .into_inner()
        .id
        .expect("candidate VPC ID");
    let root = super::vpc_prefix::seed_tenant_managed_site_prefix(
        &fixture.env,
        "remote-routing",
        "10.107.0.0/16",
        model::site_prefix::SitePrefixLifecycleState::Ready,
    )
    .await;
    let prefix = |vpc_id| forge::VpcPrefixCreationRequest {
        id: Some(carbide_uuid::vpc::VpcPrefixId::new()),
        vpc_id: Some(vpc_id),
        site_prefix_id: Some(root),
        config: Some(forge::VpcPrefixConfig {
            prefix: "10.107.1.0/24".into(),
        }),
        metadata: Some(rpc::Metadata {
            name: "remote overlap".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    fixture
        .env
        .api
        .create_vpc_prefix(Request::new(prefix(id)))
        .await?;
    let candidate_request = prefix(candidate);
    let error = fixture
        .env
        .api
        .create_vpc_prefix(Request::new(candidate_request.clone()))
        .await
        .expect_err("retained remote VNI prevents overlap");
    assert_eq!(error.code(), Code::InvalidArgument);

    // Prefix attachment also advances the VPC version. Observe the release
    // only after that attachment, just as an operator must before submitting it.
    let before_release = fixture
        .env
        .api
        .get_vpc_routing_state(Request::new(forge::VpcRoutingStateRequest { id: Some(id) }))
        .await?
        .into_inner();
    let request = forge::VpcReleaseInactiveVniRequest {
        id: Some(id),
        if_version_match: Some(before_release.version.clone()),
        expected_inactive_vni: Some(50_001),
    };
    // A blocked release must observe shutdown before it can commit intent.
    // This API owns its own token; the serving fixture remains usable afterward.
    let cancelled_api = crate::test_support::builder::TestApiBuilder::new(
        fixture.env.pool.clone(),
        fixture.env.common_pools.clone(),
        fixture.env.api.work_lock_manager_handle.clone(),
    )
    .with_resource_pool_registry(fixture.env.api.resource_pool_registry.clone())
    .with_runtime_config(fixture.env.config.clone())
    .build();
    let mut blocker = fixture.env.pool.begin().await?;
    sqlx::query(
        "SELECT allocation_id FROM resource_pool_allocations WHERE allocation_id = $1 FOR UPDATE",
    )
    .bind(original.allocation_id)
    .fetch_one(&mut *blocker)
    .await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let blocked_release = cancelled_api.release_vpc_inactive_vni(Request::new(request.clone()));
    tokio::pin!(blocked_release);
    tokio::select! {
        result = &mut blocked_release => panic!("release passed locked ownership: {result:?}"),
        _ = wait_for_blocked_queries(&fixture.env.pool, pid, "resource_pool_allocations", 1) => {},
    }
    cancelled_api.cancel_token.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), blocked_release)
        .await?
        .expect_err("shutdown stops release admission");
    assert_eq!(error.code(), Code::Cancelled);
    blocker.rollback().await?;
    assert_eq!(
        remote::load(&fixture.env.pool, original.allocation_id).await?,
        Some(original.clone())
    );
    assert_eq!(
        fixture
            .env
            .api
            .get_vpc_routing_state(Request::new(forge::VpcRoutingStateRequest { id: Some(id) }))
            .await?
            .into_inner(),
        before_release
    );
    let pause = fixture.provider.release_pause.clone();
    pause.pause_next.store(true, Ordering::SeqCst);
    let api = fixture.env.api.clone();
    let release = api.release_vpc_inactive_vni(Request::new(request.clone()));
    tokio::pin!(release);
    tokio::select! {
        result = &mut release => panic!("release completed without provider confirmation: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
    }
    let releasing = remote::load(&fixture.env.pool, original.allocation_id)
        .await?
        .expect("original release intent");
    assert_eq!(releasing.state, AllocationState::ReleaseRequested);
    assert_eq!(releasing.source, original.source);
    assert_eq!(
        receipt(&fixture, id).await?,
        internal,
        "newer active identity is unchanged"
    );
    let observed = tokio::time::timeout(
        Duration::from_secs(5),
        fixture
            .env
            .api
            .get_vpc_routing_state(Request::new(forge::VpcRoutingStateRequest { id: Some(id) })),
    )
    .await??
    .into_inner();
    assert_eq!(observed.active_vni, restored.active_vni);
    assert_eq!(
        observed.retained_allocation, restored.retained_allocation,
        "inspection preserves the original release assignment"
    );
    let current = observed.version;
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        fixture
            .env
            .api
            .create_vpc_prefix(Request::new(candidate_request.clone())),
    )
    .await?
    .expect_err("older pending release prevents overlap");
    assert_eq!(error.code(), Code::InvalidArgument);
    pause.resume.notify_one();
    let released = tokio::time::timeout(Duration::from_secs(5), release)
        .await??
        .into_inner();
    assert_eq!(released.released_inactive_vni, 50_001);
    assert_eq!(released.vpc.expect("release VPC").version, current);
    assert_eq!(
        remote::load(&fixture.env.pool, original.allocation_id)
            .await?
            .expect("released identity")
            .state,
        AllocationState::Released
    );
    fixture
        .env
        .api
        .create_vpc_prefix(Request::new(candidate_request))
        .await?;
    let mut txn = fixture.env.pool.begin().await?;
    let selected = fixture
        .env
        .api
        .resource_pool_registry
        .select(
            &mut txn,
            &fixture.env.common_pools.ethernet.pool_external_vpc_vni,
        )
        .await?
        .expect("external pool");
    txn.commit().await?;
    let unused = selected
        .prepare_exact_allocation(
            &fixture.env.pool,
            OwnerType::Vpc,
            &id.to_string(),
            "vni",
            50_002,
            &fixture.cancel,
        )
        .await?;
    crate::handlers::tenant_prefix_overlap::validate_retained_state(&fixture.env.api).await?;
    unused.cancel(&fixture.env.pool).await?;
    let cancelling = receipt(&fixture, id).await?;
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    crate::handlers::tenant_prefix_overlap::validate_retained_state(&fixture.env.api).await?;
    assert_eq!(
        fixture
            .env
            .api
            .get_vpc_routing_state(Request::new(forge::VpcRoutingStateRequest { id: Some(id) }))
            .await?
            .into_inner()
            .retained_allocation,
        None
    );
    fixture
        .remote(EXTERNAL_VPC_VNI)
        .reconcile(cancelling.allocation_id, &fixture.cancel)
        .await?;
    let error = fixture
        .env
        .api
        .release_vpc_inactive_vni(Request::new(request))
        .await
        .expect_err("the original release version cannot be replayed");
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(error.message().contains("expected version"), "{error}");
    fixture.stop().await
}

/// Final admission must run after provider preparation without holding the VPC
/// or tenant locks. An unused reservation remains recoverable after each failure.
#[crate::sqlx_test]
async fn remote_routing_failed_completion_keeps_active_and_cancels_unused(
    database: PgPool,
) -> eyre::Result<()> {
    #[derive(Clone, Copy, Debug)]
    enum Failure {
        TenantRevoked,
        FinalWrite,
        TenantPrefixAttached,
        InterfaceOverride,
        MembershipExpanded,
    }
    let fixture = vpc_fixture(database, None).await?;
    for failure in [
        Failure::TenantRevoked,
        Failure::FinalWrite,
        Failure::TenantPrefixAttached,
        Failure::InterfaceOverride,
        // Acknowledged growth is persistent, so this is the final case.
        Failure::MembershipExpanded,
    ] {
        let tenant = format!("remote-routing-{failure:?}");
        create_tenant(&fixture, &tenant, "INTERNAL").await?;
        let before = fixture
            .env
            .api
            .create_vpc(
                VpcCreationRequest::builder(&tenant)
                    .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                    .routing_profile_type("EXTERNAL")
                    .vni(50_001_u32)
                    .tonic_request(),
            )
            .await?
            .into_inner();
        let id = before.id.expect("VPC ID");
        let original = receipt(&fixture, id).await?;
        let pause = fixture.provider.allocate_pause.clone();
        pause.pause_next.store(true, Ordering::SeqCst);
        let change = fixture.env.api.change_vpc_routing_profile(Request::new(
            forge::VpcChangeRoutingProfileRequest {
                id: Some(id),
                if_version_match: Some(before.version.clone()),
                routing_profile_type: "INTERNAL".into(),
                vni: None,
            },
        ));
        tokio::pin!(change);
        tokio::select! {
            result = &mut change => panic!("routing change passed the paused provider: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
        }
        let late_machine = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x85; 32],
            MachineType::Host,
        );
        let (expected_code, expected_message) = match failure {
            Failure::TenantRevoked => {
                let query = "UPDATE tenants SET routing_profile_type = 'EXTERNAL' WHERE organization_id = $1";
                tokio::time::timeout(
                    Duration::from_secs(5),
                    sqlx::query(query).bind(&tenant).execute(&fixture.env.pool),
                )
                .await??;
                (Code::FailedPrecondition, Some("access tier"))
            }
            Failure::TenantPrefixAttached => {
                // Prefix attachment needs the overlap lock and advances the VPC
                // version. It must finish during provider I/O, then the original
                // routing observation must fail the version check first.
                let root = super::vpc_prefix::seed_tenant_managed_site_prefix(
                    &fixture.env,
                    &tenant,
                    "10.109.0.0/16",
                    model::site_prefix::SitePrefixLifecycleState::Ready,
                )
                .await;
                tokio::time::timeout(
                    Duration::from_secs(5),
                    fixture.env.api.create_vpc_prefix(Request::new(
                        forge::VpcPrefixCreationRequest {
                            id: Some(carbide_uuid::vpc::VpcPrefixId::new()),
                            vpc_id: Some(id),
                            site_prefix_id: Some(root),
                            config: Some(forge::VpcPrefixConfig {
                                prefix: "10.109.1.0/24".into(),
                            }),
                            metadata: Some(rpc::Metadata {
                                name: "late attachment".into(),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                )
                .await??;
                (Code::FailedPrecondition, Some("expected version"))
            }
            Failure::InterfaceOverride => {
                // An instance update does not advance the VPC version, so only
                // completion's attachment scan can reject this new constraint.
                let mut txn = fixture.env.pool.begin().await?;
                db::machine::create(
                    &mut txn,
                    None,
                    &late_machine,
                    ManagedHostState::Ready,
                    None,
                    CURRENT_STATE_MODEL_VERSION,
                )
                .await?;
                let mut network = InstanceNetworkConfig::for_vpc_prefix_id(
                    carbide_uuid::vpc::VpcPrefixId::new(),
                    Some(id),
                );
                network.interfaces[0].network_details = None;
                network.interfaces[0].routing_profile = Some(InstanceInterfaceRoutingProfile {
                    allowed_anycast_prefixes: vec!["198.51.100.0/24".parse()?],
                });
                sqlx::query("INSERT INTO instances (machine_id, tenant_org, network_config, nvlink_config) VALUES ($1, $2, $3, '{\"gpu_configs\": []}'::jsonb)")
                    .bind(late_machine).bind(&tenant).bind(sqlx::types::Json(network))
                    .execute(&mut *txn).await?;
                txn.commit().await?;
                (Code::FailedPrecondition, Some("interface routing override"))
            }
            Failure::MembershipExpanded => {
                let selected = fixture
                    .env
                    .api
                    .resource_pool_registry
                    .get(VPC_VNI)
                    .expect("internal binding");
                let definition = carbide_resource_pool::registration::latest_accepted(
                    &fixture.env.pool,
                    &selected.binding,
                )
                .await?
                .expect("accepted eligibility");
                let revision = definition.eligibility_revision.parse()?;
                let mut eligibility = carbide_resource_pool::value::ResourcePoolEligibility {
                    domain: model::resource_pool::ResourcePoolValueDomain::Integer,
                    entries: definition.eligibility,
                };
                eligibility.entries.push(v1::Eligibility {
                    assignment_mode: v1::AssignmentMode::Manual as i32,
                    entries: Some(v1::eligibility::Entries::Value(50_002_i64.into())),
                });
                carbide_resource_pool::registration::define(
                    &fixture.env.pool,
                    fixture.remote(VPC_VNI).provider(),
                    &selected.binding,
                    Some(revision),
                    &eligibility,
                )
                .await?;
                (
                    Code::FailedPrecondition,
                    Some("pools overlap at VNI `50002`"),
                )
            }
            Failure::FinalWrite => {
                let query = "ALTER TABLE vpcs ADD CONSTRAINT reject_remote_routing CHECK (routing_profile_type <> 'INTERNAL') NOT VALID";
                tokio::time::timeout(
                    Duration::from_secs(5),
                    sqlx::query(query).execute(&fixture.env.pool),
                )
                .await??;
                (Code::Internal, None)
            }
        };
        pause.resume.notify_one();
        let error = tokio::time::timeout(Duration::from_secs(5), change)
            .await?
            .expect_err("final admission or entity write must fail");
        assert_eq!(error.code(), expected_code, "{failure:?}: {error}");
        if let Some(message) = expected_message {
            assert!(error.message().contains(message), "{failure:?}: {error}");
        } else {
            let source = std::error::Error::source(&error)
                .expect("database status source")
                .source()
                .expect("original database error")
                .downcast_ref::<db::DatabaseError>()
                .expect("typed database error");
            assert!(
                source.violates_constraint("reject_remote_routing"),
                "{source}"
            );
        }
        let after = fixture
            .env
            .api
            .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest { vpc_ids: vec![id] }))
            .await?
            .into_inner()
            .vpcs
            .pop()
            .expect("existing VPC");
        if matches!(failure, Failure::TenantPrefixAttached) {
            assert_eq!(after.config, before.config, "{failure:?}");
            assert_eq!(after.status, before.status, "{failure:?}");
        } else {
            assert_eq!(after, before, "{failure:?}");
        }
        assert_eq!(
            remote::load(&fixture.env.pool, original.allocation_id).await?,
            Some(original.clone())
        );
        let unused = receipt(&fixture, id).await?;
        assert_eq!(unused.state, AllocationState::CancelRequested);
        if matches!(failure, Failure::FinalWrite) {
            let error = fixture
                .env
                .api
                .delete_vpc(VpcDeletionRequest::builder().id(id).tonic_request())
                .await
                .expect_err("unfinished cancellation blocks deletion");
            assert!(
                error.message().contains("unfinished VNI cleanup"),
                "{error}"
            );
            assert_eq!(receipt(&fixture, id).await?, unused);
        }
        fixture
            .remote(VPC_VNI)
            .reconcile(unused.allocation_id, &fixture.cancel)
            .await?;
        assert_eq!(
            receipt(&fixture, id).await?.state,
            AllocationState::Cancelled
        );
        match failure {
            Failure::FinalWrite => {
                sqlx::query("ALTER TABLE vpcs DROP CONSTRAINT reject_remote_routing")
                    .execute(&fixture.env.pool)
                    .await?;
            }
            Failure::TenantPrefixAttached => {
                sqlx::query("DELETE FROM network_vpc_prefixes WHERE vpc_id = $1")
                    .bind(id)
                    .execute(&fixture.env.pool)
                    .await?;
            }
            Failure::InterfaceOverride => {
                sqlx::query("DELETE FROM instances WHERE machine_id = $1")
                    .bind(late_machine)
                    .execute(&fixture.env.pool)
                    .await?;
            }
            Failure::TenantRevoked | Failure::MembershipExpanded => {}
        }
        fixture
            .env
            .api
            .delete_vpc(VpcDeletionRequest::builder().id(id).tonic_request())
            .await?;
        fixture
            .remote(EXTERNAL_VPC_VNI)
            .reconcile(original.allocation_id, &fixture.cancel)
            .await?;
        assert_eq!(fixture.provider.local.provisioned_count(), 0);
    }
    fixture.stop().await
}

/// The Integrated pool includes values added after startup in mixed comparisons;
/// checking just assigned VNIs or the original definition would miss this clash.
#[crate::sqlx_test]
async fn remote_routing_checks_integrated_growth_against_accepted_eligibility(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemotePoolFixture::start_with(database, &VPC_POOLS[..1], None).await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    create_tenant(&fixture, "mixed-routing", "INTERNAL").await?;
    let before = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("mixed-routing")
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("INTERNAL")
                .tonic_request(),
        )
        .await?
        .into_inner();
    let id = before.id.expect("VPC ID");
    let original = receipt(&fixture, id).await?;
    let mut txn = fixture.env.pool.begin().await?;
    db::resource_pool::populate(
        &fixture.env.common_pools.ethernet.pool_external_vpc_vni,
        &mut txn,
        vec![20_002],
        false,
    )
    .await?;
    txn.commit().await?;
    let request = forge::VpcChangeRoutingProfileRequest {
        id: Some(id),
        if_version_match: Some(before.version.clone()),
        routing_profile_type: "EXTERNAL".into(),
        vni: Some(50_001),
    };
    let error = fixture
        .env
        .api
        .change_vpc_routing_profile(Request::new(request.clone()))
        .await
        .expect_err("an unassigned accepted remote value overlaps grown local inventory");
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("pools overlap at VNI `20002`"),
        "{error}"
    );
    assert_eq!(receipt(&fixture, id).await?, original);
    let current = fixture
        .env
        .api
        .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest { vpc_ids: vec![id] }))
        .await?
        .into_inner();
    assert_eq!(current.vpcs, vec![before]);
    let free: bool = sqlx::query_scalar(
        "SELECT state = $2 FROM resource_pool WHERE name = $1 AND value = '50001'",
    )
    .bind(EXTERNAL_VPC_VNI)
    .bind(sqlx::types::Json(
        model::resource_pool::ResourcePoolEntryState::Free,
    ))
    .fetch_one(&fixture.env.pool)
    .await?;
    assert!(
        free,
        "rejected membership must not allocate the destination"
    );
    sqlx::query("DELETE FROM resource_pool WHERE name = $1 AND value = '20002'")
        .bind(EXTERNAL_VPC_VNI)
        .execute(&fixture.env.pool)
        .await?;
    let switched = fixture
        .env
        .api
        .change_vpc_routing_profile(Request::new(request))
        .await?
        .into_inner();
    assert_eq!(switched.active_vni, 50_001);
    assert_eq!(
        switched
            .retained_allocation
            .as_ref()
            .expect("retained remote assignment")
            .pool_name,
        VPC_VNI
    );
    assert_eq!(
        switched
            .retained_allocation
            .expect("retained remote assignment")
            .vni,
        20_001
    );
    fixture
        .env
        .api
        .release_vpc_inactive_vni(Request::new(forge::VpcReleaseInactiveVniRequest {
            id: Some(id),
            if_version_match: Some(switched.version),
            expected_inactive_vni: Some(20_001),
        }))
        .await?;
    assert_eq!(
        remote::load(&fixture.env.pool, original.allocation_id)
            .await?
            .expect("released original")
            .state,
        AllocationState::Released
    );
    fixture.stop().await
}

/// Admission expiry includes an actual VPC lock wait and dispatches no provider work.
#[crate::sqlx_test]
async fn remote_routing_admission_deadline_precedes_provider_work(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = vpc_fixture(database, None).await?;
    create_tenant(&fixture, "admission-deadline", "INTERNAL").await?;
    let before = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("admission-deadline")
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("EXTERNAL")
                .vni(50_001_u32)
                .tonic_request(),
        )
        .await?
        .into_inner();
    let id = before.id.expect("VPC ID");
    let original = receipt(&fixture, id).await?;
    let calls = fixture.provider.allocations.load(Ordering::SeqCst);
    let mut blocker = fixture.env.pool.begin().await?;
    sqlx::query("SELECT id FROM vpcs WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *blocker)
        .await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await?;
    let api = fixture.env.api.clone();
    let change =
        api.change_vpc_routing_profile(Request::new(forge::VpcChangeRoutingProfileRequest {
            id: Some(id),
            if_version_match: Some(before.version.clone()),
            routing_profile_type: "INTERNAL".into(),
            vni: None,
        }));
    tokio::pin!(change);
    tokio::select! {
        result = &mut change => panic!("admission passed a locked VPC: {result:?}"),
        _ = wait_for_blocked_queries(&fixture.env.pool, pid, "vpcs", 1) => {},
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::time::resume();
    let error = tokio::time::timeout(Duration::from_secs(5), change)
        .await?
        .expect_err("admission deadline stops the blocked query");
    assert_eq!(error.code(), Code::DeadlineExceeded);
    blocker.rollback().await?;
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), calls);
    assert_eq!(receipt(&fixture, id).await?, original);
    let current = fixture
        .env
        .api
        .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest { vpc_ids: vec![id] }))
        .await?
        .into_inner();
    assert_eq!(current.vpcs, vec![before]);
    fixture.stop().await
}

/// Routing rejects provider policy at its caller boundary, then recovers the unused value.
#[crate::sqlx_test]
async fn remote_routing_rejects_provider_policy_before_attachment(
    database: PgPool,
) -> eyre::Result<()> {
    // Only the destination is remote, so creating the source remains supported.
    let mut fixture = RemotePoolFixture::start_with(
        database,
        &VPC_POOLS[..1],
        Some(v1::RouteTargetPolicy {
            mode: v1::RouteTargetMode::PreserveGenerated as i32,
            ..Default::default()
        }),
    )
    .await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    create_tenant(&fixture, "routing-policy", "INTERNAL").await?;
    let before = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("routing-policy")
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("EXTERNAL")
                .tonic_request(),
        )
        .await?
        .into_inner();
    let id = before.id.expect("VPC ID");
    let source_vni = i32::try_from(
        before
            .status
            .as_ref()
            .expect("source VPC status")
            .vni
            .expect("source VPC VNI"),
    )?;
    let error = fixture
        .env
        .api
        .change_vpc_routing_profile(Request::new(forge::VpcChangeRoutingProfileRequest {
            id: Some(id),
            if_version_match: Some(before.version.clone()),
            routing_profile_type: "INTERNAL".into(),
            vni: None,
        }))
        .await
        .expect_err("routing cannot apply provider policy");
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("provider route-target policy is not supported"),
        "{error}"
    );
    let current = fixture
        .env
        .api
        .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest { vpc_ids: vec![id] }))
        .await?
        .into_inner();
    assert_eq!(current.vpcs, vec![before]);
    let mut txn = fixture.env.pool.begin().await?;
    assert_eq!(
        db::resource_pool::find_owned_allocation(
            &fixture.env.common_pools.ethernet.pool_external_vpc_vni,
            &mut txn,
            OwnerType::Vpc,
            &id.to_string()
        )
        .await?,
        Some(source_vni)
    );
    txn.commit().await?;
    let unused = receipt(&fixture, id).await?;
    assert_eq!(unused.state, AllocationState::CancelRequested);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture
        .remote(VPC_VNI)
        .reconcile(unused.allocation_id, &fixture.cancel)
        .await?;
    assert_eq!(
        receipt(&fixture, id).await?.state,
        AllocationState::Cancelled
    );
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    fixture.stop().await
}

/// Cancellation and expiry during completion drop the VPC lock wait before
/// saving cancellation of the unused request; the active VPC stays unchanged.
#[crate::sqlx_test]
async fn remote_routing_cancellation_drops_final_admission(database: PgPool) -> eyre::Result<()> {
    let mut fixture = vpc_fixture(database, None).await?;
    create_tenant(&fixture, "blocked-completion", "INTERNAL").await?;
    for (scenario, deadline) in [("cancellation", false), ("deadline", true)] {
        let before = fixture
            .env
            .api
            .create_vpc(
                VpcCreationRequest::builder("blocked-completion")
                    .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                    .routing_profile_type("EXTERNAL")
                    .vni(50_001_u32)
                    .tonic_request(),
            )
            .await?
            .into_inner();
        let id = before.id.expect("VPC ID");
        let original = receipt(&fixture, id).await?;
        let pause = fixture.provider.allocate_pause.clone();
        pause.pause_next.store(true, Ordering::SeqCst);
        let api = fixture.env.api.clone();
        let change =
            api.change_vpc_routing_profile(Request::new(forge::VpcChangeRoutingProfileRequest {
                id: Some(id),
                if_version_match: Some(before.version.clone()),
                routing_profile_type: "INTERNAL".into(),
                vni: None,
            }));
        tokio::pin!(change);
        tokio::select! {
            result = &mut change => panic!("routing change passed paused allocation: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
        }
        let pending = receipt(&fixture, id).await?;
        if !deadline {
            let metrics = MetricsCapture::start();
            let (competing, logs) = capture_logs_async(fixture.env.api.change_vpc_routing_profile(
                Request::new(forge::VpcChangeRoutingProfileRequest {
                    id: Some(id),
                    if_version_match: Some(before.version.clone()),
                    routing_profile_type: "INTERNAL".into(),
                    vni: Some(20_002),
                }),
            ))
            .await;
            let error =
                competing.expect_err("another foreground request cannot replace pending work");
            assert!(
                error
                    .message()
                    .contains("unfinished request for slot `vni`"),
                "{error}"
            );
            assert_eq!(receipt(&fixture, id).await?, pending);
            let failure_metric = "carbide_resource_pool_lifecycle_failures_total";
            assert!(
                logs.iter()
                    .all(|log| log.field("metric_name") != Some(failure_metric)),
                "remote slot rejection is not Integrated inventory failure: {logs:?}"
            );
            assert_eq!(
                metrics.counter_delta(
                    failure_metric,
                    &[
                        ("operation", "allocate"),
                        ("failure", "requested_value_unavailable"),
                        ("failure_policy", "required"),
                        ("allocation_mode", "requested"),
                        ("value_type", "integer"),
                    ]
                ),
                0.0
            );
            drop(metrics);
        }
        let mut blocker = fixture.env.pool.begin().await?;
        sqlx::query("SELECT id FROM vpcs WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *blocker)
            .await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        pause.resume.notify_one();
        tokio::select! {
            result = &mut change => panic!("completion passed locked VPC: {result:?}"),
            _ = wait_for_blocked_queries(&fixture.env.pool, pid, "vpcs", 1) => {},
        }
        if deadline {
            // The server is really blocked; only Tokio's whole-phase deadline moves.
            // Resume wall time before cancellation persistence performs database I/O.
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(6)).await;
            tokio::time::resume();
        } else {
            fixture.env.api.cancel_token.cancel();
        }
        let error = tokio::time::timeout(Duration::from_secs(5), change)
            .await?
            .expect_err("owning cancellation stops final SQL");
        assert_eq!(
            error.code(),
            if deadline {
                Code::DeadlineExceeded
            } else {
                Code::Cancelled
            },
            "{scenario}: {error}"
        );
        blocker.rollback().await?;
        let unused = receipt(&fixture, id).await?;
        assert_eq!(unused.state, AllocationState::CancelRequested);
        assert_eq!(
            remote::load(&fixture.env.pool, original.allocation_id).await?,
            Some(original.clone())
        );
        let current = fixture
            .env
            .api
            .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest { vpc_ids: vec![id] }))
            .await?
            .into_inner()
            .vpcs
            .pop()
            .expect("source VPC");
        assert_eq!(current, before);
        fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
        fixture
            .remote(VPC_VNI)
            .reconcile(unused.allocation_id, &fixture.cancel)
            .await?;
        assert_eq!(
            receipt(&fixture, id).await?.state,
            AllocationState::Cancelled
        );
        // Confirmed cancellation unblocks deletion of this same serving VPC.
        fixture
            .env
            .api
            .delete_vpc(VpcDeletionRequest::builder().id(id).tonic_request())
            .await?;
        fixture
            .remote(EXTERNAL_VPC_VNI)
            .reconcile(original.allocation_id, &fixture.cancel)
            .await?;
    }
    fixture.stop().await
}

/// A failed wait can follow committed release intent and an advanced version.
/// The original request remains recoverable without losing the serving VNI.
#[crate::sqlx_test]
async fn interrupted_inactive_release_preserves_intent_and_mutation_guards(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = vpc_fixture(database, None).await?;
    create_tenant(&fixture, "interrupted-release", "INTERNAL").await?;
    let created = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("interrupted-release")
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("EXTERNAL")
                .vni(50_001_u32)
                .tonic_request(),
        )
        .await?
        .into_inner();
    let id = created.id.expect("VPC ID");
    let original = receipt(&fixture, id).await?;
    let switched = fixture
        .env
        .api
        .change_vpc_routing_profile(Request::new(forge::VpcChangeRoutingProfileRequest {
            id: Some(id),
            if_version_match: Some(created.version),
            routing_profile_type: "INTERNAL".into(),
            vni: None,
        }))
        .await?
        .into_inner();
    let active = receipt(&fixture, id).await?;
    let AllocationSource::Remote { request, .. } = &active.source else {
        panic!("remote active assignment");
    };
    assert_eq!(request.selection, AllocationSelection::Automatic);
    let pause = fixture.provider.release_pause.clone();
    pause.pause_next.store(true, Ordering::SeqCst);
    let request = forge::VpcReleaseInactiveVniRequest {
        id: Some(id),
        if_version_match: Some(switched.version.clone()),
        expected_inactive_vni: Some(50_001),
    };
    let api = fixture.env.api.clone();
    let release = api.release_vpc_inactive_vni(Request::new(request.clone()));
    tokio::pin!(release);
    tokio::select! {
        result = &mut release => panic!("release passed paused confirmation: {result:?}"),
        result = tokio::time::timeout(Duration::from_secs(5), pause.entered.notified()) => result?,
    }
    fixture.env.api.cancel_token.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), release)
        .await?
        .expect_err("release wait interrupted");
    assert_eq!(error.code(), Code::Cancelled);
    let pending = remote::load(&fixture.env.pool, original.allocation_id)
        .await?
        .expect("original release intent");
    assert_eq!(pending.state, AllocationState::ReleaseRequested);
    assert_eq!(pending.source, original.source);
    assert_eq!(receipt(&fixture, id).await?, active);
    tokio::time::timeout(Duration::from_secs(5), pause.dropped.cancelled()).await?;
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    let observed = fixture
        .env
        .api
        .get_vpc_routing_state(Request::new(forge::VpcRoutingStateRequest { id: Some(id) }))
        .await?
        .into_inner();
    assert_ne!(observed.version, switched.version);
    assert_eq!(observed.active_vni, switched.active_vni);
    assert_eq!(observed.retained_allocation, switched.retained_allocation);
    let error = fixture
        .env
        .api
        .release_vpc_inactive_vni(Request::new(request))
        .await
        .expect_err("committed version excludes replay");
    assert!(error.message().contains("expected version"), "{error}");
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.env.api.change_vpc_routing_profile(Request::new(
            forge::VpcChangeRoutingProfileRequest {
                id: Some(id),
                if_version_match: Some(observed.version),
                routing_profile_type: "EXTERNAL".into(),
                vni: None,
            },
        )),
    )
    .await?
    .expect_err("cleanup alone blocks routing with no attached prefix");
    assert!(
        error.message().contains("unfinished VNI cleanup"),
        "{error}"
    );
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        fixture
            .env
            .api
            .delete_vpc(VpcDeletionRequest::builder().id(id).tonic_request()),
    )
    .await?
    .expect_err("cleanup alone blocks deletion");
    assert!(
        error.message().contains("unfinished VNI cleanup"),
        "{error}"
    );
    fixture
        .remote(EXTERNAL_VPC_VNI)
        .reconcile(original.allocation_id, &fixture.cancel)
        .await?;
    assert_eq!(
        remote::load(&fixture.env.pool, original.allocation_id)
            .await?
            .expect("confirmed release")
            .state,
        AllocationState::Released
    );
    assert_eq!(receipt(&fixture, id).await?, active);
    let after = fixture
        .env
        .api
        .get_vpc_routing_state(Request::new(forge::VpcRoutingStateRequest { id: Some(id) }))
        .await?
        .into_inner();
    assert_eq!(after.retained_allocation, None);
    fixture
        .env
        .api
        .delete_vpc(VpcDeletionRequest::builder().id(id).tonic_request())
        .await?;
    fixture
        .remote(VPC_VNI)
        .reconcile(active.allocation_id, &fixture.cancel)
        .await?;
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    fixture.stop().await
}

/// Pending growth and unavailable Stats cannot replace acknowledged membership.
/// Only acknowledgement of the same free collision makes the transition invalid.
#[crate::sqlx_test]
async fn routing_membership_uses_acknowledged_remote_eligibility(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = vpc_fixture(database, None).await?;
    create_tenant(&fixture, "acknowledged-membership", "INTERNAL").await?;
    let before = fixture
        .env
        .api
        .create_vpc(
            VpcCreationRequest::builder("acknowledged-membership")
                .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
                .routing_profile_type("EXTERNAL")
                .vni(50_001_u32)
                .tonic_request(),
        )
        .await?
        .into_inner();
    let id = before.id.expect("VPC ID");
    let selected = fixture
        .env
        .api
        .resource_pool_registry
        .get(VPC_VNI)
        .expect("internal binding");
    let definition =
        carbide_resource_pool::registration::latest_accepted(&fixture.env.pool, &selected.binding)
            .await?
            .expect("accepted eligibility");
    let revision = definition.eligibility_revision.parse()?;
    let mut eligibility = carbide_resource_pool::value::ResourcePoolEligibility {
        domain: model::resource_pool::ResourcePoolValueDomain::Integer,
        entries: definition.eligibility,
    };
    eligibility.entries.push(v1::Eligibility {
        assignment_mode: v1::AssignmentMode::Manual as i32,
        entries: Some(v1::eligibility::Entries::Value(50_002_i64.into())),
    });
    fixture
        .provider
        .stats_unavailable
        .store(true, Ordering::SeqCst);
    fixture.provider.reject_define_call.store(
        fixture.provider.defines.load(Ordering::SeqCst) + 1,
        Ordering::SeqCst,
    );
    carbide_resource_pool::registration::define(
        &fixture.env.pool,
        fixture.remote(VPC_VNI).provider(),
        &selected.binding,
        Some(revision),
        &eligibility,
    )
    .await
    .expect_err("pending definition is not yet acknowledged");
    let switched = fixture
        .env
        .api
        .change_vpc_routing_profile(Request::new(forge::VpcChangeRoutingProfileRequest {
            id: Some(id),
            if_version_match: Some(before.version),
            routing_profile_type: "INTERNAL".into(),
            vni: None,
        }))
        .await?
        .into_inner();
    fixture
        .provider
        .reject_define_call
        .store(0, Ordering::SeqCst);
    carbide_resource_pool::registration::define(
        &fixture.env.pool,
        fixture.remote(VPC_VNI).provider(),
        &selected.binding,
        Some(revision),
        &eligibility,
    )
    .await?;
    let held = receipt(&fixture, id).await?;
    let error = fixture
        .env
        .api
        .change_vpc_routing_profile(Request::new(forge::VpcChangeRoutingProfileRequest {
            id: Some(id),
            if_version_match: Some(switched.version.clone()),
            routing_profile_type: "EXTERNAL".into(),
            vni: Some(50_001),
        }))
        .await
        .expect_err("acknowledged collision rejects routing between remote pools");
    assert!(
        error.message().contains("pools overlap at VNI `50002`"),
        "{error}"
    );
    assert_eq!(receipt(&fixture, id).await?, held);
    assert_eq!(
        fixture
            .env
            .api
            .get_vpc_routing_state(Request::new(forge::VpcRoutingStateRequest { id: Some(id) }))
            .await?
            .into_inner(),
        switched
    );
    fixture.stop().await
}
