// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use carbide_instrument::testing::{CapturedLog, capture_logs_async};
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_resource_pool::grpc::GrpcResourcePoolError;
use carbide_resource_pool::remote::RemoteResourcePool;
use carbide_resource_pool::value::ResourcePoolEligibility;
use carbide_resource_pool::{PoolDefinitionSource, registration};
use carbide_resource_pool_api::testing::LocalProvider;
use carbide_resource_pool_api::v1;
use carbide_resource_pool_api::v1::resource_pool_server::{ResourcePool, ResourcePoolServer};
use carbide_uuid::vpc::VpcId;
use db::resource_pool::remote;
use model::resource_pool::allocation::{
    AllocationSource, AllocationState, AssignmentMode, ResourcePoolAllocation, ResourcePoolValue,
};
use model::resource_pool::common::{EXTERNAL_VPC_VNI, VPC_VNI};
use model::resource_pool::remote::{AllocationResult, AllocationSelection, CompletionAuthority};
use model::resource_pool::{
    OwnerType, Range, ResourcePoolBackendKind, ResourcePoolDef, ResourcePoolSelection,
    ResourcePoolType, ResourcePoolValueDomain,
};
use model::vpc::VpcDefinition;
use rpc::forge;
use rpc::forge::forge_server::Forge;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tonic::{Code, Request, Response, Status};
use uuid::Uuid;

use crate::cfg::resource_pool::ResourcePoolBackendConfig;
use crate::db_init;
use crate::resource_pool::ResourcePoolMonitor;
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_test_env_with_overrides,
};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::{VpcCreationRequest, VpcDeletionRequest};

mod startup;

const AUTHORITY: &str = "test-fabric";
const PROVIDER_POOL: &str = "fabric-vnis";

/// `RemoteFixture` injects a provider below the production selection gate.
/// It exercises the shared startup helpers and public VPC handlers without
/// enabling remote selections through `start_runtime`.
struct RemoteFixture {
    env: TestEnv,
    provider: ObservedProvider,
    selections: HashMap<String, ResourcePoolSelection>,
    definitions: HashMap<String, ResourcePoolDef>,
    cancel: CancellationToken,
    server: JoinSet<Result<(), tonic::transport::Error>>,
}

impl RemoteFixture {
    /// Prepare the database and provider without submitting a definition, so
    /// registration tests can interrupt the first startup attempt.
    async fn start(database: PgPool) -> eyre::Result<Self> {
        Self::start_with_policy(database, None).await
    }

    /// Keep policy in the provider's saved assignment so rejection tests exercise
    /// the shared response accessor and can recover the same result for cleanup.
    async fn start_with_policy(
        database: PgPool,
        policy: Option<v1::RouteTargetPolicy>,
    ) -> eyre::Result<Self> {
        let mut env = create_test_env_with_overrides(
            database,
            TestEnvOverrides::no_network_segments().with_fnn_config(None),
        )
        .await;
        let provider = ObservedProvider {
            local: LocalProvider::new(
                v1::PoolBinding {
                    authority_id: AUTHORITY.into(),
                    pool_id: PROVIDER_POOL.into(),
                    binding_id: Uuid::new_v4().to_string(),
                    logical_pool: VPC_VNI.into(),
                },
                v1::ValueDomain::Integer,
                [
                    -1_i64,
                    2_147_483_648,
                    20_001,
                    20_002,
                    20_003,
                    50_001,
                    50_002,
                ]
                .into_iter()
                .map(Into::into)
                .collect(),
                policy,
            )?,
            allocations: Arc::default(),
            cancellations: Arc::default(),
            releases: Arc::default(),
            defines: Arc::default(),
            describe_changed_pool: Arc::default(),
            stats_unavailable: Arc::default(),
            lose_define_reply: Arc::default(),
            cancel_unavailable: Arc::default(),
            allocation_reply: ReplyGate::default(),
            recovery_reply: ReplyGate::default(),
            cancellation_reply: ReplyGate::default(),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}", listener.local_addr()?).parse()?;
        let cancel = CancellationToken::new();
        let mut server = JoinSet::new();
        server.spawn(
            Server::builder()
                .add_service(ResourcePoolServer::new(provider.clone()))
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(listener),
                    cancel.clone().cancelled_owned(),
                ),
        );
        let mut config = env.config.as_ref().clone();
        config.resource_pool_backends.insert(
            "fabric".into(),
            ResourcePoolBackendConfig::Grpc {
                endpoint,
                tls: None,
            },
        );
        env.config = Arc::new(config);

        // This fixture has no VPCs. Replace only its unused VNI pools; the
        // other Integrated pools still exercise normal CommonPools readiness.
        let existing_vpcs: i64 = sqlx::query_scalar("SELECT count(*) FROM vpcs")
            .fetch_one(&env.pool)
            .await?;
        assert_eq!(existing_vpcs, 0);
        let mut txn = env.pool.begin().await?;
        let mut definitions = db::resource_pool::all_stored_defs(&mut *txn).await?;
        for (name, start, auto_assign) in
            [(VPC_VNI, 20_001, true), (EXTERNAL_VPC_VNI, 50_001, false)]
        {
            let receipts: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM resource_pool_allocations WHERE pool_name = $1",
            )
            .bind(name)
            .fetch_one(&mut *txn)
            .await?;
            assert_eq!(receipts, 0);
            for query in [
                "DELETE FROM resource_pool WHERE name = $1",
                "DELETE FROM resource_pool_binding WHERE pool_name = $1",
                "DELETE FROM resource_pool_def WHERE name = $1",
            ] {
                sqlx::query(query).bind(name).execute(&mut *txn).await?;
            }
            definitions.insert(
                name.into(),
                ResourcePoolDef {
                    pool_type: ResourcePoolType::Integer,
                    ranges: vec![Range {
                        start: start.to_string(),
                        end: (start + 2).to_string(),
                        auto_assign,
                    }],
                    prefix: None,
                    delegate_prefix_len: None,
                },
            );
        }
        txn.commit().await?;
        let selections = [VPC_VNI, EXTERNAL_VPC_VNI]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    ResourcePoolSelection {
                        pool_name: name.into(),
                        value_domain: ResourcePoolValueDomain::Integer,
                        backend_name: Some("fabric".into()),
                        backend_kind: ResourcePoolBackendKind::Grpc,
                        authority_id: AUTHORITY.into(),
                        remote_pool: PROVIDER_POOL.into(),
                    },
                )
            })
            .collect();
        Ok(Self {
            env,
            provider,
            selections,
            definitions,
            cancel,
            server,
        })
    }

    /// Start with accepted definitions for ordinary VPC lifecycle tests.
    async fn new(database: PgPool) -> eyre::Result<Self> {
        let mut fixture = Self::start(database).await?;
        fixture
            .initialize(PoolDefinitionSource::SeedConfiguration)
            .await?;
        Ok(fixture)
    }

    /// Rebuild the API and coordinators over the same database and provider.
    /// Return the seed reconciler's logs so tests can check its drift diagnostics.
    /// No recovery task runs until a test explicitly asks it to, so deletion
    /// assertions can observe committed release intent before provider cleanup.
    async fn initialize(&mut self, source: PoolDefinitionSource) -> eyre::Result<Vec<CapturedLog>> {
        let mut monitor = ResourcePoolMonitor::new(
            self.env.pool.clone(),
            &self.env.config.resource_pool_backends,
        )?;
        let mut txn = self.env.pool.begin().await?;
        let registry = monitor
            .resolve_registry(
                source,
                &mut txn,
                &self.selections,
                &self.env.api.work_lock_manager_handle,
            )
            .await?;
        let logs = if matches!(source, PoolDefinitionSource::SeedConfiguration) {
            // Capture the drift producer without changing the subscriber used
            // by provider preparation and its blocking tasks.
            let (result, logs) = capture_logs_async(db::resource_pool::reconcile_pool_defs(
                &mut txn,
                &self.definitions,
            ))
            .await;
            result?;
            logs
        } else {
            Vec::new()
        };
        txn.commit().await?;
        monitor
            .register_pools(&registry, source, &self.cancel)
            .await?;
        self.env.common_pools = db::resource_pool::create_common_pools(
            self.env.pool.clone(),
            ["default".to_owned()].into(),
        )
        .await?;
        self.env.api = Arc::new(
            TestApiBuilder::new(
                self.env.pool.clone(),
                self.env.common_pools.clone(),
                self.env.api.work_lock_manager_handle.clone(),
            )
            .with_resource_pool_registry(registry)
            .with_runtime_config(self.env.config.clone())
            .build(),
        );
        Ok(logs)
    }

    /// Access the coordinator installed in the API rather than constructing a
    /// second allocation path for the test.
    fn remote(&self, name: &str) -> &RemoteResourcePool {
        self.env
            .api
            .resource_pool_registry
            .get(name)
            .expect("registered test pool")
            .backend
            .remote()
            .expect("remote test pool")
    }

    /// Load the saved request, including confirmed cleanup records that active
    /// allocation listings intentionally omit.
    async fn receipt(&self, vpc_id: VpcId) -> eyre::Result<ResourcePoolAllocation> {
        Ok(remote::latest_for_owner_slot(
            &self.env.pool,
            OwnerType::Vpc,
            &vpc_id.to_string(),
            "vni",
        )
        .await?
        .expect("VPC allocation request"))
    }

    /// Create configured VPCs with the fixture's current registry and token.
    async fn create_configured(
        &self,
        definitions: &HashMap<String, VpcDefinition>,
    ) -> Result<(), crate::CarbideError> {
        db_init::create_initial_vpcs(
            &self.env.pool,
            definitions,
            &self.env.common_pools.ethernet.pool_vpc_vni,
            &self.env.api.resource_pool_registry,
            &self.cancel,
        )
        .await
    }

    /// Join the local provider before its database fixture is torn down.
    async fn stop(mut self) -> eyre::Result<()> {
        self.cancel.cancel();
        while let Some(result) =
            tokio::time::timeout(Duration::from_secs(5), self.server.join_next()).await?
        {
            result??;
        }
        Ok(())
    }
}

/// `ReplyGate` pauses one reply after the provider has completed its work.
/// Tests can then interrupt the caller or change admission before Core sees it.
#[derive(Clone, Default)]
struct ReplyGate {
    held: Arc<AtomicBool>,
    reached: Arc<Notify>,
    resume: Arc<Notify>,
}

impl ReplyGate {
    /// Pause the next successful call; later calls continue normally.
    fn hold_next(&self) {
        self.held.store(true, Ordering::SeqCst);
    }

    /// Signal that provider work finished before waiting for the test to resume.
    async fn pause(&self) {
        if self.held.swap(false, Ordering::SeqCst) {
            self.reached.notify_one();
            self.resume.notified().await;
        }
    }
}

/// `ObservedProvider` injects faults around the real local provider and counts
/// calls so a retry cannot quietly make another reservation.
#[derive(Clone)]
struct ObservedProvider {
    local: LocalProvider,
    allocations: Arc<AtomicUsize>,
    cancellations: Arc<AtomicUsize>,
    releases: Arc<AtomicUsize>,
    defines: Arc<AtomicUsize>,
    describe_changed_pool: Arc<AtomicBool>,
    stats_unavailable: Arc<AtomicBool>,
    lose_define_reply: Arc<AtomicBool>,
    cancel_unavailable: Arc<AtomicBool>,
    allocation_reply: ReplyGate,
    recovery_reply: ReplyGate,
    cancellation_reply: ReplyGate,
}

#[tonic::async_trait]
impl ResourcePool for ObservedProvider {
    async fn describe(
        &self,
        request: Request<v1::DescribeRequest>,
    ) -> Result<Response<v1::DescribeResponse>, Status> {
        let mut response = self.local.describe(request).await?;
        // Simulate an alias resolving to a different pool after restart.
        if self.describe_changed_pool.load(Ordering::SeqCst) {
            response.get_mut().pool_id = "replacement-vnis".into();
        }
        Ok(response)
    }

    async fn define(
        &self,
        request: Request<v1::DefineRequest>,
    ) -> Result<Response<v1::DefineResponse>, Status> {
        self.defines.fetch_add(1, Ordering::SeqCst);
        let lose_reply = self.lose_define_reply.load(Ordering::SeqCst)
            && request
                .get_ref()
                .pool
                .as_ref()
                .is_some_and(|pool| pool.logical_pool == VPC_VNI);
        let response = self.local.define(request).await?;
        // The provider accepted the definition, but Core never gets its reply.
        if lose_reply {
            return Err(Status::unavailable("injected loss of registration reply"));
        }
        Ok(response)
    }

    async fn allocate(
        &self,
        request: Request<v1::AllocateRequest>,
    ) -> Result<Response<v1::AllocateResponse>, Status> {
        self.allocations.fetch_add(1, Ordering::SeqCst);
        let response = self.local.allocate(request).await?;
        self.allocation_reply.pause().await;
        Ok(response)
    }

    async fn allocate_exact(
        &self,
        request: Request<v1::AllocateExactRequest>,
    ) -> Result<Response<v1::AllocateExactResponse>, Status> {
        self.allocations.fetch_add(1, Ordering::SeqCst);
        let response = self.local.allocate_exact(request).await?;
        self.allocation_reply.pause().await;
        Ok(response)
    }

    async fn recover(
        &self,
        request: Request<v1::RecoverRequest>,
    ) -> Result<Response<v1::RecoverResponse>, Status> {
        let response = self.local.recover(request).await?;
        self.recovery_reply.pause().await;
        Ok(response)
    }

    async fn cancel(
        &self,
        request: Request<v1::CancelRequest>,
    ) -> Result<Response<v1::CancelResponse>, Status> {
        self.cancellations.fetch_add(1, Ordering::SeqCst);
        if self.cancel_unavailable.load(Ordering::SeqCst) {
            return Err(Status::unavailable("injected cancellation failure"));
        }
        let response = self.local.cancel(request).await?;
        self.cancellation_reply.pause().await;
        Ok(response)
    }

    async fn release(
        &self,
        request: Request<v1::ReleaseRequest>,
    ) -> Result<Response<v1::ReleaseResponse>, Status> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        self.local.release(request).await
    }

    async fn lookup(
        &self,
        request: Request<v1::LookupRequest>,
    ) -> Result<Response<v1::LookupResponse>, Status> {
        self.local.lookup(request).await
    }

    async fn stats(
        &self,
        request: Request<v1::StatsRequest>,
    ) -> Result<Response<v1::StatsResponse>, Status> {
        if self.stats_unavailable.load(Ordering::SeqCst) {
            return Err(Status::unavailable("injected statistics failure"));
        }
        self.local.stats(request).await
    }
}

/// Both VNI pools use the API's shared create/find/delete flow, including the
/// saved receipt needed to release an allocation after the API restarts.
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
        fail_release_write: bool,
    }
    let mut fixture = RemoteFixture::new(database).await?;
    for case in [
        // Internal VPCs generate their ID and ask for an automatic value.
        Case {
            profile: "INTERNAL",
            pool: VPC_VNI,
            requested: None,
            expected_vni: 20_001,
            mode: AssignmentMode::Automatic,
            fail_release_write: true,
        },
        // External VPCs preserve the requested ID and manual value.
        Case {
            profile: "EXTERNAL",
            pool: EXTERNAL_VPC_VNI,
            requested: Some(50_001),
            expected_vni: 50_001,
            mode: AssignmentMode::Manual,
            fail_release_write: false,
        },
    ] {
        fixture
            .provider
            .stats_unavailable
            .store(true, Ordering::SeqCst);
        fixture
            .env
            .api
            .create_tenant(Request::new(forge::CreateTenantRequest {
                organization_id: case.profile.into(),
                routing_profile_type: Some(case.profile.into()),
                metadata: Some(rpc::Metadata {
                    name: case.profile.into(),
                    ..Default::default()
                }),
            }))
            .await?;
        // Neither pool treats unavailable statistics as missing capacity.
        {
            let binding = &fixture
                .env
                .api
                .resource_pool_registry
                .get(case.pool)
                .expect("binding")
                .binding;
            let definition = registration::latest_accepted(&fixture.env.pool, binding)
                .await?
                .expect("accepted definition");
            let error = fixture
                .remote(case.pool)
                .provider()
                .stats(v1::StatsRequest {
                    pool: definition.pool,
                    eligibility_revision: definition.eligibility_revision,
                })
                .await
                .expect_err("injected statistics failure");
            assert!(
                matches!(error, GrpcResourcePoolError::Rpc(status) if status.code() == Code::Unavailable)
            );
        }
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
        let request = request.tonic_request();
        if requested_id.is_none() {
            assert!(request.get_ref().id.is_none());
            assert!(request.get_ref().metadata.is_none());
        }
        let before_create: chrono::DateTime<chrono::Utc> =
            sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&fixture.env.pool)
                .await?;
        let created = fixture.env.api.create_vpc(request).await?.into_inner();
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
        let found = fixture
            .env
            .api
            .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest {
                vpc_ids: vec![vpc_id],
            }))
            .await?
            .into_inner();
        assert_eq!(found.vpcs.as_slice(), std::slice::from_ref(&created));
        let routing = fixture
            .env
            .api
            .get_vpc_routing_state(Request::new(forge::VpcRoutingStateRequest {
                id: Some(vpc_id),
            }))
            .await?
            .into_inner();
        assert_eq!(
            routing,
            forge::VpcRoutingState {
                id: Some(vpc_id),
                version: created.version.clone(),
                routing_profile_type: Some(case.profile.into()),
                active_vni: case.expected_vni,
                retained_allocation: None,
            }
        );

        let attached = fixture.receipt(vpc_id).await?;
        assert_eq!(attached.state, AllocationState::Attached);
        assert_eq!(attached.pool_name, case.pool);
        assert_eq!(attached.owner_type, OwnerType::Vpc);
        assert_eq!(attached.owner_id, vpc_id.to_string());
        let AllocationSource::Remote {
            binding_id,
            request,
            result: Some(AllocationResult::Allocated(assignment)),
        } = &attached.source
        else {
            panic!("VPC must have an assigned remote receipt: {attached:?}");
        };
        assert_eq!(
            *binding_id,
            fixture
                .env
                .api
                .resource_pool_registry
                .get(case.pool)
                .expect("binding")
                .binding
                .binding_id
        );
        assert_eq!(request.authority_id, AUTHORITY);
        assert_eq!(request.pool_id, PROVIDER_POOL);
        assert_eq!(request.eligibility_revision, "1");
        assert_eq!(request.slot, "vni");
        let CompletionAuthority::Foreground {
            attempt_id,
            attach_before,
        } = &request.completion_authority
        else {
            panic!("API creation must belong to the waiting caller: {request:?}");
        };
        assert!(!attempt_id.is_nil());
        assert!(*attach_before >= before_create + chrono::Duration::seconds(60));
        assert!(*attach_before <= attached.created_at + chrono::Duration::seconds(60));
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
        let local_rows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM resource_pool WHERE name IN ($1, $2)")
                .bind(VPC_VNI)
                .bind(EXTERNAL_VPC_VNI)
                .fetch_one(&fixture.env.pool)
                .await?;
        assert_eq!(local_rows, 0);

        // Deletion commits release intent before recovery contacts the provider.
        let releases = fixture.provider.releases.load(Ordering::SeqCst);
        if case.fail_release_write {
            // A failed release-intent write must also undo the VPC deletion.
            sqlx::query("ALTER TABLE resource_pool_allocations ADD CONSTRAINT reject_test_release CHECK (state <> 'release_requested')")
                .execute(&fixture.env.pool)
                .await?;
            let error = fixture
                .env
                .api
                .delete_vpc(VpcDeletionRequest::builder().id(vpc_id).tonic_request())
                .await
                .expect_err("injected release-intent write failure");
            assert_eq!(error.code(), Code::Internal);
            let found = fixture
                .env
                .api
                .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest {
                    vpc_ids: vec![vpc_id],
                }))
                .await?
                .into_inner();
            assert_eq!(found.vpcs.as_slice(), std::slice::from_ref(&created));
            assert_eq!(fixture.receipt(vpc_id).await?, attached);
            assert!(
                fixture
                    .provider
                    .local
                    .is_provisioned(&assignment.allocation_id)
            );
            assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), releases);
            sqlx::query(
                "ALTER TABLE resource_pool_allocations DROP CONSTRAINT reject_test_release",
            )
            .execute(&fixture.env.pool)
            .await?;
        }
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
        let releasing = fixture.receipt(vpc_id).await?;
        assert_eq!(releasing.state, AllocationState::ReleaseRequested);
        assert_eq!(releasing.source, attached.source);
        assert_eq!(fixture.provider.releases.load(Ordering::SeqCst), releases);
        assert!(
            fixture
                .provider
                .local
                .is_provisioned(&assignment.allocation_id)
        );

        fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
        fixture
            .remote(case.pool)
            .reconcile(attached.allocation_id, &fixture.cancel)
            .await?;
        assert_eq!(
            fixture.receipt(vpc_id).await?.state,
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

/// A failed entity write cannot roll back provider work. After recovery cancels
/// the reservation, a new API call can reuse the VPC ID with a new allocation.
#[crate::sqlx_test]
async fn remote_vpc_insert_failure_recovers_cancellation(database: PgPool) -> eyre::Result<()> {
    let mut fixture = RemoteFixture::new(database).await?;
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
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM vpcs WHERE id = $1")
        .bind(vpc_id)
        .fetch_one(&fixture.env.pool)
        .await?;
    assert_eq!(count, 0);
    let cancelled = fixture.receipt(vpc_id).await?;
    assert_eq!(cancelled.state, AllocationState::CancelRequested);
    let AllocationSource::Remote {
        binding_id,
        request: original_request,
        result: Some(AllocationResult::Allocated(original_assignment)),
    } = &cancelled.source
    else {
        panic!("failed creation must retain its cleanup identity: {cancelled:?}");
    };
    assert_eq!(original_request.generation, 0);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);

    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    fixture
        .remote(VPC_VNI)
        .reconcile(cancelled.allocation_id, &fixture.cancel)
        .await?;
    let completed = fixture.receipt(vpc_id).await?;
    assert_eq!(completed.state, AllocationState::Cancelled);
    assert_eq!(completed.source, cancelled.source);
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM vpcs WHERE id = $1")
        .bind(vpc_id)
        .fetch_one(&fixture.env.pool)
        .await?;
    assert_eq!(count, 0);

    // The same VPC ID can start over once cleanup is confirmed. Keep the old
    // request cancelled instead of reopening it or attaching its released VNI.
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
    let attached = fixture.receipt(vpc_id).await?;
    assert_eq!(attached.state, AllocationState::Attached);
    assert_ne!(attached.allocation_id, completed.allocation_id);
    assert_eq!(attached.owner_id, vpc_id.to_string());
    let AllocationSource::Remote {
        binding_id: new_binding,
        request: new_request,
        result: Some(AllocationResult::Allocated(new_assignment)),
    } = &attached.source
    else {
        panic!("retry must attach a new allocation: {attached:?}");
    };
    assert_eq!(new_binding, binding_id);
    assert_eq!(new_request.generation, original_request.generation + 1);
    assert_eq!(new_request.slot, original_request.slot);
    assert_eq!(new_request.selection, original_request.selection);
    assert_ne!(
        new_assignment.allocation_id,
        original_assignment.allocation_id
    );
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&new_assignment.allocation_id)
    );
    assert!(
        !fixture
            .provider
            .local
            .is_provisioned(&original_assignment.allocation_id)
    );
    assert_eq!(
        remote::load(&fixture.env.pool, completed.allocation_id).await?,
        Some(completed)
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}

/// A reservation cannot attach after admission changes or the API stops.
/// Shutdown also bounds the final entity write after the provider succeeds.
#[crate::sqlx_test]
async fn remote_vpc_cancels_after_admission_changes_or_shutdown(
    database: PgPool,
) -> eyre::Result<()> {
    #[derive(Clone, Copy)]
    enum Interruption {
        TenantRoutingChanged,
        ApiStopped,
        FinalInsertBlocked,
    }

    let mut fixture = RemoteFixture::new(database).await?;
    for (tenant, interruption) in [
        // Recheck tenant admission after releasing its locks for provider I/O.
        ("changing-tenant", Interruption::TenantRoutingChanged),
        // Shutdown must also interrupt SQL after the provider has returned.
        ("blocked-insert-tenant", Interruption::FinalInsertBlocked),
        // Shutdown cancels the request, which another caller cannot adopt.
        ("stopping-tenant", Interruption::ApiStopped),
    ] {
        if fixture.env.api.cancel_token.is_cancelled() {
            fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
        }
        fixture
            .env
            .api
            .create_tenant(Request::new(forge::CreateTenantRequest {
                organization_id: tenant.into(),
                routing_profile_type: Some("INTERNAL".into()),
                metadata: Some(rpc::Metadata {
                    name: tenant.into(),
                    ..Default::default()
                }),
            }))
            .await?;
        let vpc_id = VpcId::new();
        let request = VpcCreationRequest::builder(tenant)
            .id(vpc_id)
            .network_virtualization_type(forge::VpcVirtualizationType::Fnn as i32)
            .tonic_request()
            .into_inner();
        let allocations = fixture.provider.allocations.load(Ordering::SeqCst);
        fixture.provider.allocation_reply.hold_next();
        let mut resolved_before_cancel = None;
        {
            let creation = fixture.env.api.create_vpc(Request::new(request.clone()));
            tokio::pin!(creation);
            tokio::select! {
                result = &mut creation => {
                    panic!("creation finished before its allocation reply: {result:?}");
                }
                reached = tokio::time::timeout(
                    Duration::from_secs(5),
                    fixture.provider.allocation_reply.reached.notified(),
                ) => {
                    reached?;
                }
            }
            assert_eq!(fixture.provider.local.provisioned_count(), 1);
            match interruption {
                Interruption::TenantRoutingChanged => {
                    // This write would block if admission still held the tenant
                    // row while waiting for the provider's allocation reply.
                    let query = "UPDATE tenants SET routing_profile_type = 'EXTERNAL'
                        WHERE organization_id = $1";
                    let changed = tokio::time::timeout(
                        Duration::from_secs(5),
                        sqlx::query(query).bind(tenant).execute(&fixture.env.pool),
                    )
                    .await??;
                    assert_eq!(changed.rows_affected(), 1);
                    fixture.provider.allocation_reply.resume.notify_one();
                    let error = tokio::time::timeout(Duration::from_secs(5), creation)
                        .await?
                        .expect_err("tenant changed allocation pools");
                    assert_eq!(error.code(), Code::FailedPrecondition);
                    assert!(error.message().contains("routing changed"), "{error}");
                }
                Interruption::ApiStopped => {
                    let pending = fixture.receipt(vpc_id).await?;
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
                    assert_eq!(fixture.receipt(vpc_id).await?, pending);
                    assert_eq!(
                        fixture.provider.allocations.load(Ordering::SeqCst),
                        allocations + 1
                    );

                    // Cancel while the provider still withholds its reply. The
                    // handler must save cleanup before returning `Cancelled`.
                    fixture.env.api.cancel_token.cancel();
                    let error = tokio::time::timeout(Duration::from_secs(5), creation)
                        .await?
                        .expect_err("API stopped during allocation");
                    assert_eq!(error.code(), Code::Cancelled);
                    fixture.provider.allocation_reply.resume.notify_one();
                }
                Interruption::FinalInsertBlocked => {
                    let mut blocker = fixture.env.pool.begin().await?;
                    sqlx::query("LOCK TABLE vpcs IN SHARE MODE")
                        .execute(&mut *blocker)
                        .await?;
                    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                        .fetch_one(&mut *blocker)
                        .await?;
                    fixture.provider.allocation_reply.resume.notify_one();
                    let waiter_pid = tokio::select! {
                        result = &mut creation => {
                            panic!("creation did not wait for its INSERT: {result:?}");
                        }
                        blocked = tokio::time::timeout(
                            Duration::from_secs(5),
                            wait_for_blocked_query(
                                &fixture.env.pool,
                                blocker_pid,
                                "INSERT INTO vpcs",
                            ),
                        ) => blocked?,
                    };
                    let pending = fixture.receipt(vpc_id).await?;
                    assert_eq!(pending.state, AllocationState::Pending);
                    assert!(matches!(
                        pending.source,
                        AllocationSource::Remote {
                            result: Some(AllocationResult::Allocated(_)),
                            ..
                        }
                    ));
                    resolved_before_cancel = Some(pending);
                    fixture.env.api.cancel_token.cancel();
                    let error = tokio::time::timeout(Duration::from_secs(12), creation)
                        .await?
                        .expect_err("API stopped during the final INSERT");
                    assert_eq!(error.code(), Code::Cancelled);
                    // Keep the blocker until the abandoned query stops, so
                    // releasing it cannot hide a late entity commit.
                    // `database_transaction` limits statements to five seconds;
                    // allow one more second for the queued rollback.
                    tokio::time::timeout(Duration::from_secs(6), async {
                        loop {
                            let query = "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                                WHERE pid = $1 AND state <> 'idle')";
                            let busy: bool = sqlx::query_scalar(query)
                                .bind(waiter_pid)
                                .fetch_one(&fixture.env.pool)
                                .await?;
                            if !busy {
                                return Ok::<_, sqlx::Error>(());
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await??;
                    blocker.rollback().await?;
                }
            }
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM vpcs WHERE id = $1")
            .bind(vpc_id)
            .fetch_one(&fixture.env.pool)
            .await?;
        assert_eq!(count, 0);
        let mut cancelling = fixture.receipt(vpc_id).await?;
        if let Some(pending) = resolved_before_cancel {
            assert_eq!(cancelling.allocation_id, pending.allocation_id);
            assert_eq!(cancelling.source, pending.source);
            assert!(matches!(
                cancelling.state,
                AllocationState::Pending | AllocationState::CancelRequested
            ));
            // Cleanup can time out waiting for the abandoned transaction's row
            // lock. Expire that foreground attempt so recovery owns cancellation.
            if cancelling.state == AllocationState::Pending {
                let expired = sqlx::query(
                    "UPDATE resource_pool_allocations SET remote_request = jsonb_set(
                        remote_request, '{completion_authority,attach_before}',
                        to_jsonb(clock_timestamp() - interval '1 second'))
                     WHERE allocation_id = $1",
                )
                .bind(cancelling.allocation_id)
                .execute(&fixture.env.pool)
                .await?;
                assert_eq!(expired.rows_affected(), 1);
                cancelling = fixture.receipt(vpc_id).await?;
            }
        } else {
            assert_eq!(cancelling.state, AllocationState::CancelRequested);
        }
        match (&interruption, &cancelling.source) {
            (
                Interruption::TenantRoutingChanged | Interruption::FinalInsertBlocked,
                AllocationSource::Remote {
                    result: Some(AllocationResult::Allocated(_)),
                    ..
                },
            ) => {}
            (Interruption::ApiStopped, AllocationSource::Remote { result: None, .. }) => {}
            _ => {
                panic!("cancellation must retain the reply state observed by Core: {cancelling:?}")
            }
        }
        assert_eq!(fixture.provider.local.provisioned_count(), 1);
        // `reconcile` uses `fixture.cancel`, so cleanup can finish after API
        // shutdown even when Core never received the provider's value.
        fixture
            .remote(VPC_VNI)
            .reconcile(cancelling.allocation_id, &fixture.cancel)
            .await?;
        let cancelled = fixture.receipt(vpc_id).await?;
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

/// Configured creation has a saved request independent of an API caller.
/// A failed second INSERT rolls back both attachments and entities; restart
/// finishes the same VPCs, tenants, and reservations.
/// Recreating a deleted VPC starts with fresh IDs instead of reviving that request.
#[crate::sqlx_test]
async fn configured_remote_vpc_reuses_identity_after_insert_failure(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemoteFixture::start(database).await?;
    // Leave room to recreate the first VPC while its old release is queued.
    fixture
        .definitions
        .get_mut(VPC_VNI)
        .expect("VPC pool")
        .ranges[0]
        .end = "20004".into();
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let definitions = ["configured-remote", "configured-remote-second"]
        .into_iter()
        .map(|name| {
            (
                name.into(),
                VpcDefinition {
                    organization_id: None,
                    network_virtualization_type: VpcVirtualizationType::Flat,
                    routing_profile_type: None,
                    routing_profile_overrides: None,
                    vni: None,
                },
            )
        })
        .collect();
    sqlx::query(
        "ALTER TABLE vpcs ADD CONSTRAINT reject_remote_vpc CHECK (name <> 'configured-remote-second')",
    )
    .execute(&fixture.env.pool)
    .await?;
    let error = fixture
        .create_configured(&definitions)
        .await
        .expect_err("injected configured VPC insert failure");
    assert!(error.to_string().contains("reject_remote_vpc"), "{error}");
    let pending = db::vpc::configured_creation::list(&fixture.env.pool).await?;
    assert_eq!(pending.len(), 2);
    let mut allocations = Vec::new();
    for creation in &pending {
        let allocated = fixture.receipt(creation.vpc_id).await?;
        assert_eq!(allocated.state, AllocationState::Pending);
        let AllocationSource::Remote {
            request,
            result: Some(AllocationResult::Allocated(assignment)),
            ..
        } = &allocated.source
        else {
            panic!("configured VPC must retain its allocation: {allocated:?}");
        };
        assert_eq!(
            request.completion_authority,
            CompletionAuthority::Controller {
                desire_id: creation.vpc_id.into()
            }
        );
        assert!(
            fixture
                .provider
                .local
                .is_provisioned(&assignment.allocation_id)
        );
        allocations.push(allocated);
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM vpcs")
        .fetch_one(&fixture.env.pool)
        .await?;
    assert_eq!(count, 0);

    // Once the database accepts the row, startup attaches the stored result.
    // Rebuilding the API drops any in-memory allocation state from the first try.
    sqlx::query("ALTER TABLE vpcs DROP CONSTRAINT reject_remote_vpc")
        .execute(&fixture.env.pool)
        .await?;
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    fixture.create_configured(&definitions).await?;
    for (creation, allocated) in pending.iter().zip(&allocations) {
        let completed = fixture.receipt(creation.vpc_id).await?;
        assert_eq!(completed.allocation_id, allocated.allocation_id);
        assert_eq!(completed.state, AllocationState::Attached);
        assert_eq!(completed.source, allocated.source);
    }
    assert!(
        db::vpc::configured_creation::list(&fixture.env.pool)
            .await?
            .is_empty()
    );
    let vpcs: Vec<(VpcId, String, i32)> =
        sqlx::query_as("SELECT id, organization_id, vni FROM vpcs ORDER BY name")
            .fetch_all(&fixture.env.pool)
            .await?;
    assert_eq!(
        vpcs,
        pending
            .iter()
            .zip([20_001, 20_002])
            .map(|(creation, vni)| {
                (
                    creation.vpc_id,
                    creation.tenant_organization_id.clone(),
                    vni,
                )
            })
            .collect::<Vec<_>>()
    );
    let creation = &pending[0];
    let allocated = &allocations[0];
    let found = fixture
        .env
        .api
        .find_vpcs_by_ids(Request::new(forge::VpcsByIdsRequest {
            vpc_ids: vec![creation.vpc_id],
        }))
        .await?
        .into_inner();
    assert_eq!(found.vpcs.len(), 1);
    let created = &found.vpcs[0];
    assert_eq!(
        created.config.as_ref().expect("VPC config").vni,
        Some(20_001)
    );
    assert_eq!(
        created.status.as_ref().expect("VPC status").vni,
        Some(20_001)
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.local.provisioned_count(), 2);

    // Leave the old release queued while recreating the same configured name.
    // The new VPC must have its own request, even before the old VNI is free.
    fixture
        .env
        .api
        .delete_vpc(
            VpcDeletionRequest::builder()
                .id(creation.vpc_id)
                .tonic_request(),
        )
        .await?;
    let deleted: bool = sqlx::query_scalar("SELECT deleted IS NOT NULL FROM vpcs WHERE id = $1")
        .bind(creation.vpc_id)
        .fetch_one(&fixture.env.pool)
        .await?;
    assert!(deleted);
    assert_eq!(
        fixture.receipt(creation.vpc_id).await?.state,
        AllocationState::ReleaseRequested
    );
    fixture.create_configured(&definitions).await?;
    let mut active_vpcs = db::vpc::find_by_name(&fixture.env.pool, "configured-remote").await?;
    assert_eq!(active_vpcs.len(), 1);
    let recreated = active_vpcs.pop().expect("recreated configured VPC");
    assert_ne!(recreated.id, creation.vpc_id);
    assert_ne!(
        recreated.config.tenant_organization_id,
        creation.tenant_organization_id
    );
    assert_eq!(recreated.status.vni, Some(20_003));
    let new_receipt = fixture.receipt(recreated.id).await?;
    assert_eq!(new_receipt.state, AllocationState::Attached);
    assert_ne!(new_receipt.allocation_id, allocated.allocation_id);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.provider.local.provisioned_count(), 3);
    assert!(
        db::vpc::configured_creation::list(&fixture.env.pool)
            .await?
            .is_empty()
    );

    fixture
        .remote(VPC_VNI)
        .reconcile(allocated.allocation_id, &fixture.cancel)
        .await?;
    assert_eq!(
        fixture.receipt(creation.vpc_id).await?.state,
        AllocationState::Released
    );
    assert_eq!(fixture.receipt(recreated.id).await?, new_receipt);
    assert_eq!(fixture.provider.local.provisioned_count(), 2);
    fixture.stop().await
}

/// A configured request has no foreground deadline. Startup retains its ID
/// after a failed cancellation and retries cleanup without a recovery scanner.
#[crate::sqlx_test]
async fn removed_configured_remote_vpc_cancels_its_unattached_allocation(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemoteFixture::new(database).await?;
    let definitions = HashMap::from([(
        "removed-configured".into(),
        VpcDefinition {
            organization_id: None,
            network_virtualization_type: VpcVirtualizationType::Flat,
            routing_profile_type: None,
            routing_profile_overrides: None,
            vni: None,
        },
    )]);
    sqlx::query(
        "ALTER TABLE vpcs ADD CONSTRAINT reject_remote_vpc CHECK (name <> 'removed-configured')",
    )
    .execute(&fixture.env.pool)
    .await?;
    let error = fixture
        .create_configured(&definitions)
        .await
        .expect_err("injected configured VPC insert failure");
    assert!(error.to_string().contains("reject_remote_vpc"), "{error}");
    let mut pending = db::vpc::configured_creation::list(&fixture.env.pool).await?;
    assert_eq!(pending.len(), 1);
    let creation = pending.pop().expect("pending configured VPC");
    let allocated = fixture.receipt(creation.vpc_id).await?;
    assert_eq!(allocated.state, AllocationState::Pending);
    let AllocationSource::Remote {
        request,
        result: Some(AllocationResult::Allocated(_)),
        ..
    } = &allocated.source
    else {
        panic!("configured VPC must retain its allocation: {allocated:?}");
    };
    assert_eq!(
        request.completion_authority,
        CompletionAuthority::Controller {
            desire_id: creation.vpc_id.into(),
        }
    );

    // Startup now receives no such definition, but the provider cannot cancel
    // yet. Keep the old ID so the next startup can retry that same reservation.
    fixture
        .provider
        .cancel_unavailable
        .store(true, Ordering::SeqCst);
    let error = fixture
        .create_configured(&HashMap::new())
        .await
        .expect_err("injected cancellation failure");
    assert_eq!(Status::from(error).code(), Code::Unavailable);
    assert_eq!(
        db::vpc::configured_creation::list(&fixture.env.pool).await?,
        vec![creation.clone()],
    );
    let cancelling = fixture.receipt(creation.vpc_id).await?;
    assert_eq!(cancelling.allocation_id, allocated.allocation_id);
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    assert_eq!(cancelling.source, allocated.source);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    assert_eq!(fixture.provider.cancellations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);

    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
    fixture
        .provider
        .cancel_unavailable
        .store(false, Ordering::SeqCst);
    fixture.create_configured(&HashMap::new()).await?;
    assert!(
        db::vpc::configured_creation::list(&fixture.env.pool)
            .await?
            .is_empty()
    );
    let cancelled = fixture.receipt(creation.vpc_id).await?;
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.source, allocated.source);
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    assert_eq!(fixture.provider.cancellations.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM vpcs")
        .fetch_one(&fixture.env.pool)
        .await?;
    assert_eq!(count, 0);
    fixture.stop().await
}

/// A lost reply must not change a configured VPC's identity or registration
/// revision. Recovery and another startup may finish the original reservation
/// while this startup waits; all three must agree on the same request.
/// Startup also honors cancellation while waiting for its configured row lock.
#[crate::sqlx_test]
async fn configured_remote_vpc_recovers_lost_reply_during_concurrent_startup(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemoteFixture::start(database).await?;
    fixture
        .definitions
        .get_mut(VPC_VNI)
        .expect("VNI definition")
        .ranges[0]
        .auto_assign = false;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let definitions = HashMap::from([(
        "lost-configured".into(),
        VpcDefinition {
            organization_id: None,
            network_virtualization_type: VpcVirtualizationType::Flat,
            routing_profile_type: None,
            routing_profile_overrides: None,
            vni: Some(20_001),
        },
    )]);
    fixture.provider.allocation_reply.hold_next();
    {
        let creation = fixture.create_configured(&definitions);
        tokio::pin!(creation);
        tokio::select! {
            result = &mut creation => {
                panic!("creation finished before its allocation reply: {result:?}");
            }
            reached = tokio::time::timeout(
                Duration::from_secs(5),
                fixture.provider.allocation_reply.reached.notified(),
            ) => {
                reached?;
            }
        }
        // Drop the caller before it sees the reply, then rebuild to test replay.
        // This releases its lease; it does not simulate a process crash or the
        // lease that would survive one.
    }
    fixture.provider.allocation_reply.resume.notify_one();
    let mut creations = db::vpc::configured_creation::list(&fixture.env.pool).await?;
    assert_eq!(creations.len(), 1);
    let creation = creations.pop().expect("saved configured VPC");
    let pending = fixture.receipt(creation.vpc_id).await?;
    assert_eq!(pending.state, AllocationState::Pending);
    assert_eq!(pending.owner_id, creation.vpc_id.to_string());
    let AllocationSource::Remote {
        binding_id,
        request,
        result: None,
    } = &pending.source
    else {
        panic!("the allocation reply must not have reached Core: {pending:?}");
    };
    assert_eq!(request.eligibility_revision, "1");
    assert_eq!(request.slot, "vni");
    assert_eq!(
        request.selection,
        AllocationSelection::Requested(ResourcePoolValue::Integer(20_001))
    );
    assert_eq!(
        request.completion_authority,
        CompletionAuthority::Controller {
            desire_id: creation.vpc_id.into(),
        }
    );
    let binding = fixture
        .env
        .api
        .resource_pool_registry
        .get(VPC_VNI)
        .expect("VNI binding")
        .binding
        .clone();
    assert_eq!(*binding_id, binding.binding_id);
    let definition = registration::latest_accepted(&fixture.env.pool, &binding)
        .await?
        .expect("accepted definition");
    let recovered = fixture
        .provider
        .local
        .recover(Request::new(v1::RecoverRequest {
            operation_identity: Some(v1::OperationIdentity {
                pool: definition.pool,
                request_id: pending.allocation_id.to_string(),
            }),
        }))
        .await?
        .into_inner();
    let Some(v1::operation_status::State::Allocated(provider_assignment)) =
        recovered.operation_status.and_then(|status| status.state)
    else {
        panic!("the provider must have reserved the requested VNI");
    };
    assert_eq!(provider_assignment.value, Some(20_001_i64.into()));
    assert_eq!(
        provider_assignment.assignment_mode,
        v1::AssignmentMode::Manual as i32
    );
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&provider_assignment.allocation_id)
    );
    assert!(
        db::vpc::find_by_name(&fixture.env.pool, &creation.name)
            .await?
            .is_empty()
    );

    // Grow the accepted definition before rebuilding Core. Replaying the saved
    // request must still use revision 1 after revision 2 is accepted.
    let mut grown = fixture.definitions[VPC_VNI].clone();
    grown.ranges.push(Range {
        start: "30001".into(),
        end: "30002".into(),
        auto_assign: false,
    });
    let accepted = registration::define(
        &fixture.env.pool,
        fixture.remote(VPC_VNI).provider(),
        &binding,
        Some(1),
        &ResourcePoolEligibility::try_from(grown)?,
    )
    .await?;
    assert_eq!(accepted.eligibility_revision, "2");
    fixture.initialize(PoolDefinitionSource::ListenOnly).await?;

    // A blocked configured row must not prevent shutdown. Observe the actual
    // SQL wait, cancel the startup, and keep the blocker until its abandoned
    // query stops so a leaked connection cannot make this pass accidentally.
    // `database_transaction` limits statements to five seconds; allow one more
    // second for the queued rollback.
    {
        let mut blocker = fixture.env.pool.begin().await?;
        db::vpc::configured_creation::lock(&mut blocker, &creation.name)
            .await?
            .expect("saved configured VPC");
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        let cancel = CancellationToken::new();
        let startup = db_init::create_initial_vpcs(
            &fixture.env.pool,
            &definitions,
            &fixture.env.common_pools.ethernet.pool_vpc_vni,
            &fixture.env.api.resource_pool_registry,
            &cancel,
        );
        tokio::pin!(startup);
        let waiter_pid = tokio::select! {
            result = &mut startup => {
                panic!("startup did not wait for the configured row: {result:?}");
            }
            blocked = tokio::time::timeout(
                Duration::from_secs(5),
                wait_for_blocked_query(
                    &fixture.env.pool,
                    blocker_pid,
                    "configured_vpc_creation",
                ),
            ) => blocked?,
        };
        cancel.cancel();
        let error = tokio::time::timeout(Duration::from_secs(1), startup)
            .await?
            .expect_err("cancelled configured startup");
        assert_eq!(Status::from(error).code(), Code::Cancelled);
        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let query = "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                    WHERE pid = $1 AND state <> 'idle')";
                let busy: bool = sqlx::query_scalar(query)
                    .bind(waiter_pid)
                    .fetch_one(&fixture.env.pool)
                    .await?;
                if !busy {
                    return Ok::<_, sqlx::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        blocker.rollback().await?;
    }
    assert_eq!(fixture.receipt(creation.vpc_id).await?, pending);
    assert_eq!(
        db::vpc::configured_creation::list(&fixture.env.pool).await?,
        vec![creation.clone()]
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.provider.cancellations.load(Ordering::SeqCst), 0);

    // Recovery holds the real lease while its Recover reply is paused. The
    // blocked manager INSERT below proves replay tried to acquire that lease.
    fixture.provider.recovery_reply.hold_next();
    {
        let recovery = fixture
            .remote(VPC_VNI)
            .reconcile(pending.allocation_id, &fixture.cancel);
        tokio::pin!(recovery);
        tokio::select! {
            result = &mut recovery => {
                panic!("recovery finished before its reply: {result:?}");
            }
            reached = tokio::time::timeout(
                Duration::from_secs(5),
                fixture.provider.recovery_reply.reached.notified(),
            ) => {
                reached?;
            }
        }
        let mut blocker = fixture.env.pool.begin().await?;
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        // `FOR KEY SHARE` blocks acquisition's `worker_id` change, the same
        // lock mode that `WorkLock::fence_transaction` uses.
        let locked: String =
            sqlx::query_scalar("SELECT work_key FROM work_locks WHERE work_key = $1 FOR KEY SHARE")
                .bind(format!("resource-pool:{}", pending.allocation_id))
                .fetch_one(&mut *blocker)
                .await?;
        assert_eq!(locked, format!("resource-pool:{}", pending.allocation_id));
        let replay = fixture.create_configured(&definitions);
        tokio::pin!(replay);
        tokio::select! {
            result = &mut replay => {
                panic!("replay did not wait for the allocation lease: {result:?}");
            }
            blocked = tokio::time::timeout(
                Duration::from_secs(5),
                wait_for_blocked_query(
                    &fixture.env.pool,
                    blocker_pid,
                    "INSERT INTO work_locks",
                ),
            ) => {
                blocked?;
            }
        }
        blocker.commit().await?;

        // The manager processes commands in order. Completing this unrelated
        // acquire proves the manager sent `WorkAlreadyLocked` before recovery
        // releases its lease, rather than merely starting after recovery ended.
        let barrier = tokio::select! {
            result = &mut replay => {
                panic!("replay failed while recovery held its lease: {result:?}");
            }
            barrier = fixture.env.api.work_lock_manager_handle.try_acquire_lock(
                "test-replay-barrier".into(),
            ) => barrier?,
        };
        barrier.release().await?;
        fixture.provider.recovery_reply.resume.notify_one();
        tokio::time::timeout(Duration::from_secs(5), recovery).await??;

        // Both startup calls must succeed with the saved request. A caller
        // observing the completed VPC must accept it without another allocation.
        fixture.create_configured(&definitions).await?;
        tokio::time::timeout(Duration::from_secs(5), replay).await??;
    }
    let attached = fixture.receipt(creation.vpc_id).await?;
    assert_eq!(attached.allocation_id, pending.allocation_id);
    assert_eq!(attached.owner_id, pending.owner_id);
    assert_eq!(attached.state, AllocationState::Attached);
    let AllocationSource::Remote {
        binding_id: attached_binding,
        request: attached_request,
        result: Some(AllocationResult::Allocated(assignment)),
    } = &attached.source
    else {
        panic!("recovered configured VPC must have its original assignment: {attached:?}");
    };
    assert_eq!(attached_binding, binding_id);
    assert_eq!(attached_request, request);
    assert_eq!(assignment.allocation_id, provider_assignment.allocation_id);
    assert_eq!(assignment.value, ResourcePoolValue::Integer(20_001));
    assert_eq!(assignment.assignment_mode, AssignmentMode::Manual);
    let vpcs = db::vpc::find_by_name(&fixture.env.pool, &creation.name).await?;
    assert_eq!(vpcs.len(), 1);
    assert_eq!(vpcs[0].id, creation.vpc_id);
    assert_eq!(
        vpcs[0].config.tenant_organization_id,
        creation.tenant_organization_id
    );
    assert_eq!(vpcs[0].config.vni, Some(20_001));
    assert_eq!(vpcs[0].status.vni, Some(20_001));
    assert!(
        db::vpc::configured_creation::list(&fixture.env.pool)
            .await?
            .is_empty()
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.provider.cancellations.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}

/// Integer providers can return values that fail typed conversion or VNI bounds.
/// Startup cancels either assignment and keeps the configured VPC's identity
/// while retrying cleanup, without asking for another allocation.
#[crate::sqlx_test]
async fn configured_remote_vpc_cancels_an_invalid_vni(database: PgPool) -> eyre::Result<()> {
    enum Eligibility {
        Seeded,
        GrownFrom(i64),
    }

    struct Case {
        value: i64,
        expected_error: &'static str,
        eligibility: Eligibility,
    }

    let mut fixture = RemoteFixture::start(database).await?;
    // Start with only the overflow value eligible. Adding -1 in the second
    // case selects it first in provider inventory without shrinking the pool.
    fixture
        .definitions
        .get_mut(VPC_VNI)
        .expect("VNI definition")
        .ranges
        .clear();
    let definitions = HashMap::from([(
        "invalid-vni".into(),
        VpcDefinition {
            organization_id: None,
            network_virtualization_type: VpcVirtualizationType::Flat,
            routing_profile_type: None,
            routing_profile_overrides: None,
            vni: None,
        },
    )]);
    for Case {
        value,
        expected_error,
        eligibility,
    } in [
        // Typed resolution fails before the VPC's own range validation runs.
        Case {
            value: 2_147_483_648,
            expected_error: "cannot be represented by i32",
            eligibility: Eligibility::Seeded,
        },
        // This fits i32 but fails the VPC-specific bounds check.
        Case {
            value: -1,
            expected_error: "between 1 and 16777215",
            eligibility: Eligibility::GrownFrom(1),
        },
    ] {
        fixture
            .definitions
            .get_mut(VPC_VNI)
            .expect("VNI definition")
            .ranges
            .push(Range {
                start: value.to_string(),
                end: (value + 1).to_string(),
                auto_assign: true,
            });
        if let Eligibility::GrownFrom(previous_revision) = eligibility {
            // Seed drift does not change eligibility. Explicitly grow the
            // accepted definition so the second case can allocate -1.
            let binding = &fixture
                .env
                .api
                .resource_pool_registry
                .get(VPC_VNI)
                .expect("VNI binding")
                .binding;
            let accepted = registration::define(
                &fixture.env.pool,
                fixture.remote(VPC_VNI).provider(),
                binding,
                Some(previous_revision),
                &ResourcePoolEligibility::try_from(fixture.definitions[VPC_VNI].clone())?,
            )
            .await?;
            assert_eq!(accepted.eligibility_revision, "2");
            fixture.initialize(PoolDefinitionSource::ListenOnly).await?;
        } else {
            fixture
                .initialize(PoolDefinitionSource::SeedConfiguration)
                .await?;
        }
        let allocations = fixture.provider.allocations.load(Ordering::SeqCst);
        let error = fixture
            .create_configured(&definitions)
            .await
            .expect_err("provider returned an unusable VNI");
        let error = Status::from(error);
        assert_eq!(error.code(), Code::Internal, "{value}: {error}");
        assert!(error.message().contains(expected_error), "{value}: {error}");
        assert!(
            db::vpc::find_by_name(&fixture.env.pool, "invalid-vni")
                .await?
                .is_empty()
        );
        let mut creations = db::vpc::configured_creation::list(&fixture.env.pool).await?;
        assert_eq!(creations.len(), 1);
        let creation = creations.pop().expect("saved invalid configured VPC");
        let cancelling = fixture.receipt(creation.vpc_id).await?;
        assert_eq!(cancelling.state, AllocationState::CancelRequested);
        let AllocationSource::Remote {
            result: Some(AllocationResult::Allocated(assignment)),
            ..
        } = &cancelling.source
        else {
            panic!("invalid VNI must retain its cleanup identity: {cancelling:?}");
        };
        assert_eq!(assignment.value, ResourcePoolValue::Integer(value));
        assert!(
            fixture
                .provider
                .local
                .is_provisioned(&assignment.allocation_id)
        );

        // The same configuration must finish cleanup and retain the saved VPC
        // and tenant IDs for a later attempt, rather than starting a new request.
        let error = fixture
            .create_configured(&definitions)
            .await
            .expect_err("startup must retry after finishing VNI cleanup");
        let error = Status::from(error);
        assert_eq!(error.code(), Code::FailedPrecondition, "{value}: {error}");
        assert!(
            error.message().contains("retry after VNI cleanup"),
            "{error}"
        );
        assert_eq!(
            db::vpc::configured_creation::list(&fixture.env.pool).await?,
            vec![creation.clone()],
        );
        let cancelled = fixture.receipt(creation.vpc_id).await?;
        assert_eq!(cancelled.allocation_id, cancelling.allocation_id);
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

/// Provider routing instructions must reach VPC validation through the shared
/// allocation result. Reject them without attaching or losing cleanup identity.
#[crate::sqlx_test]
async fn remote_vpc_cancels_an_unsupported_provider_policy(database: PgPool) -> eyre::Result<()> {
    let mut fixture = RemoteFixture::start_with_policy(
        database,
        Some(v1::RouteTargetPolicy {
            mode: v1::RouteTargetMode::PreserveGenerated as i32,
            ..Default::default()
        }),
    )
    .await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
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
    let cancelling = fixture.receipt(vpc_id).await?;
    assert_eq!(cancelling.state, AllocationState::CancelRequested);
    let AllocationSource::Remote {
        result: Some(AllocationResult::Allocated(assignment)),
        ..
    } = &cancelling.source
    else {
        panic!("rejected policy must keep the provider assignment: {cancelling:?}");
    };
    assert_eq!(assignment.value, ResourcePoolValue::Integer(20_001));
    assert_eq!(
        assignment
            .route_target_policy
            .as_ref()
            .expect("saved provider policy")
            .mode,
        model::resource_pool::remote::ProviderRouteTargetMode::Preserve,
    );
    assert!(
        fixture
            .provider
            .local
            .is_provisioned(&assignment.allocation_id)
    );

    fixture
        .remote(VPC_VNI)
        .reconcile(cancelling.allocation_id, &fixture.cancel)
        .await?;
    let cancelled = fixture.receipt(vpc_id).await?;
    assert_eq!(cancelled.allocation_id, cancelling.allocation_id);
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.source, cancelling.source);
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    fixture.stop().await
}

/// A changed configured VNI cannot reuse an unfinished selection. Cancel the
/// old request before a later startup creates the VPC with fresh owner IDs.
#[crate::sqlx_test]
async fn changed_configured_vni_cancels_before_starting_a_new_request(
    database: PgPool,
) -> eyre::Result<()> {
    let mut fixture = RemoteFixture::start(database).await?;
    fixture
        .definitions
        .get_mut(VPC_VNI)
        .expect("VNI definition")
        .ranges[0]
        .auto_assign = false;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    let mut definitions = HashMap::from([(
        "changed-configured".into(),
        VpcDefinition {
            organization_id: None,
            network_virtualization_type: VpcVirtualizationType::Flat,
            routing_profile_type: None,
            routing_profile_overrides: None,
            vni: Some(20_001),
        },
    )]);
    sqlx::query(
        "ALTER TABLE vpcs ADD CONSTRAINT reject_changed_vpc CHECK (name <> 'changed-configured')",
    )
    .execute(&fixture.env.pool)
    .await?;
    let error = fixture
        .create_configured(&definitions)
        .await
        .expect_err("leave the original configured request unfinished");
    assert!(error.to_string().contains("reject_changed_vpc"), "{error}");
    let mut creations = db::vpc::configured_creation::list(&fixture.env.pool).await?;
    assert_eq!(creations.len(), 1);
    let original = creations.pop().expect("original configured creation");
    let pending = fixture.receipt(original.vpc_id).await?;
    assert_eq!(pending.state, AllocationState::Pending);

    // The provider already reserved 20001. Changing only the requested VNI
    // must take `RequestChanged` rather than replaying that saved assignment.
    definitions
        .get_mut("changed-configured")
        .expect("configured VPC")
        .vni = Some(20_002);
    let error = fixture
        .create_configured(&definitions)
        .await
        .expect_err("changed input requires cleanup before a new request");
    let error = Status::from(error);
    assert_eq!(error.code(), Code::FailedPrecondition);
    assert!(
        error.message().contains("retry after VNI cleanup"),
        "{error}"
    );
    let cancelled = fixture.receipt(original.vpc_id).await?;
    assert_eq!(cancelled.allocation_id, pending.allocation_id);
    assert_eq!(cancelled.state, AllocationState::Cancelled);
    assert_eq!(cancelled.source, pending.source);
    assert!(
        db::vpc::configured_creation::list(&fixture.env.pool)
            .await?
            .is_empty()
    );
    assert!(
        db::vpc::find_by_name(&fixture.env.pool, "changed-configured")
            .await?
            .is_empty()
    );
    assert_eq!(fixture.provider.local.provisioned_count(), 0);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);

    sqlx::query("ALTER TABLE vpcs DROP CONSTRAINT reject_changed_vpc")
        .execute(&fixture.env.pool)
        .await?;
    fixture.create_configured(&definitions).await?;
    let mut vpcs = db::vpc::find_by_name(&fixture.env.pool, "changed-configured").await?;
    assert_eq!(vpcs.len(), 1);
    let created = vpcs.pop().expect("replacement VPC");
    assert_ne!(created.id, original.vpc_id);
    assert_ne!(
        created.config.tenant_organization_id,
        original.tenant_organization_id
    );
    assert_eq!(created.status.vni, Some(20_002));
    let attached = fixture.receipt(created.id).await?;
    assert_eq!(attached.state, AllocationState::Attached);
    assert_ne!(attached.allocation_id, pending.allocation_id);
    let AllocationSource::Remote { request, .. } = &attached.source else {
        panic!("replacement must keep its new request: {attached:?}");
    };
    assert_eq!(
        request.selection,
        AllocationSelection::Requested(ResourcePoolValue::Integer(20_002))
    );
    assert_eq!(
        remote::load(&fixture.env.pool, cancelled.allocation_id).await?,
        Some(cancelled)
    );
    assert!(
        db::vpc::configured_creation::list(&fixture.env.pool)
            .await?
            .is_empty()
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}

/// Cleanup may finish after another startup has replaced the saved creation.
/// Its final identity check must preserve the replacement and its generated IDs.
#[crate::sqlx_test]
async fn configured_vpc_cleanup_preserves_a_replacement_creation(
    database: PgPool,
) -> eyre::Result<()> {
    let fixture = RemoteFixture::new(database).await?;
    let definitions = HashMap::from([(
        "replaced-configured".into(),
        VpcDefinition {
            organization_id: None,
            network_virtualization_type: VpcVirtualizationType::Flat,
            routing_profile_type: None,
            routing_profile_overrides: None,
            vni: None,
        },
    )]);
    sqlx::query(
        "ALTER TABLE vpcs ADD CONSTRAINT reject_replaced_vpc CHECK (name <> 'replaced-configured')",
    )
    .execute(&fixture.env.pool)
    .await?;
    let error = fixture
        .create_configured(&definitions)
        .await
        .expect_err("leave a configured request for cleanup");
    assert!(error.to_string().contains("reject_replaced_vpc"), "{error}");
    let mut creations = db::vpc::configured_creation::list(&fixture.env.pool).await?;
    assert_eq!(creations.len(), 1);
    let original = creations.pop().expect("original configured creation");
    let pending = fixture.receipt(original.vpc_id).await?;
    assert_eq!(pending.state, AllocationState::Pending);

    fixture.provider.cancellation_reply.hold_next();
    let removed_definitions = HashMap::new();
    let (replacement, cancelled) = {
        let cleanup = fixture.create_configured(&removed_definitions);
        tokio::pin!(cleanup);
        tokio::select! {
            result = &mut cleanup => {
                panic!("cleanup finished before its provider reply: {result:?}");
            }
            reached = tokio::time::timeout(
                Duration::from_secs(5),
                fixture.provider.cancellation_reply.reached.notified(),
            ) => {
                reached?;
            }
        }

        // Cleanup has passed its first identity check. Hold the row until its
        // provider result is saved and the final identity check actually waits.
        let mut blocker = fixture.env.pool.begin().await?;
        assert_eq!(
            db::vpc::configured_creation::lock(&mut blocker, &original.name).await?,
            Some(original.clone())
        );
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        fixture.provider.cancellation_reply.resume.notify_one();
        tokio::select! {
            result = &mut cleanup => {
                panic!("cleanup bypassed the configured row lock: {result:?}");
            }
            blocked = tokio::time::timeout(
                Duration::from_secs(5),
                wait_for_blocked_query(
                    &fixture.env.pool,
                    blocker_pid,
                    "configured_vpc_creation",
                ),
            ) => {
                blocked?;
            }
        }
        let cancelled = fixture.receipt(original.vpc_id).await?;
        assert_eq!(cancelled.state, AllocationState::Cancelled);
        assert_eq!(cancelled.source, pending.source);
        assert_eq!(fixture.provider.local.provisioned_count(), 0);

        // Another startup may now forget the confirmed cancellation and save a
        // fresh creation. Perform those writes under its lock before resuming
        // the older cleanup; replacing an unfinished request would be invalid.
        db::vpc::configured_creation::remove(&mut blocker, &original.name).await?;
        let replacement =
            db::vpc::configured_creation::begin(&mut blocker, &original.name, None).await?;
        assert_ne!(replacement.vpc_id, original.vpc_id);
        assert_ne!(
            replacement.tenant_organization_id,
            original.tenant_organization_id
        );
        blocker.commit().await?;
        tokio::time::timeout(Duration::from_secs(5), cleanup).await??;
        (replacement, cancelled)
    };
    assert_eq!(
        db::vpc::configured_creation::list(&fixture.env.pool).await?,
        vec![replacement.clone()]
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.provider.cancellations.load(Ordering::SeqCst), 1);
    assert!(
        db::vpc::find_by_name(&fixture.env.pool, &original.name)
            .await?
            .is_empty()
    );

    // The next configured startup must finish the preserved replacement without
    // reviving the old allocation when the provider can reuse its released value.
    sqlx::query("ALTER TABLE vpcs DROP CONSTRAINT reject_replaced_vpc")
        .execute(&fixture.env.pool)
        .await?;
    fixture.create_configured(&definitions).await?;
    let mut vpcs = db::vpc::find_by_name(&fixture.env.pool, &original.name).await?;
    assert_eq!(vpcs.len(), 1);
    let created = vpcs.pop().expect("replacement VPC");
    assert_eq!(created.id, replacement.vpc_id);
    assert_eq!(
        created.config.tenant_organization_id,
        replacement.tenant_organization_id
    );
    let attached = fixture.receipt(created.id).await?;
    assert_eq!(attached.state, AllocationState::Attached);
    assert_ne!(attached.allocation_id, cancelled.allocation_id);
    assert_eq!(
        remote::load(&fixture.env.pool, cancelled.allocation_id).await?,
        Some(cancelled)
    );
    assert!(
        db::vpc::configured_creation::list(&fixture.env.pool)
            .await?
            .is_empty()
    );
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.local.provisioned_count(), 1);
    fixture.stop().await
}
