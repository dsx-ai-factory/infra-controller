// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Provider and API fixtures for resource-pool startup tests.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_instrument::testing::{CapturedLog, capture_logs_async};
use carbide_resource_pool::PoolDefinitionSource;
use carbide_resource_pool::remote::RemoteResourcePool;
use carbide_resource_pool_api::testing::LocalProvider;
use carbide_resource_pool_api::v1;
use carbide_resource_pool_api::v1::resource_pool_server::{ResourcePool, ResourcePoolServer};
use model::resource_pool::{
    Range, ResourcePoolBackendKind, ResourcePoolDef, ResourcePoolSelection, ResourcePoolType,
    ResourcePoolValueDomain,
};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::cfg::resource_pool::ResourcePoolBackendConfig;
use crate::resource_pool::ResourcePoolMonitor;
use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_test_env_with_overrides,
};

/// `REMOTE_POOL` allocates automatically from the test provider.
pub(in crate::tests) const REMOTE_POOL: &str = "test-remote-integer";
/// `SECOND_REMOTE_POOL` shares the provider but requires a requested value.
pub(in crate::tests) const SECOND_REMOTE_POOL: &str = "test-remote-manual";

const AUTHORITY: &str = "test-fabric";
const PROVIDER_POOL: &str = "test-integers";

/// `RemotePoolFixture` injects a provider below the production selection gate.
/// Its ordinary integer pools leave mandatory Integrated pools unchanged, so
/// startup tests exercise provider registration without changing VPC behavior.
pub(in crate::tests) struct RemotePoolFixture {
    pub(in crate::tests) env: TestEnv,
    pub(in crate::tests) provider: ObservedProvider,
    pub(in crate::tests) selections: HashMap<String, ResourcePoolSelection>,
    pub(in crate::tests) definitions: HashMap<String, ResourcePoolDef>,
    pub(in crate::tests) cancel: CancellationToken,
    server: JoinSet<Result<(), tonic::transport::Error>>,
}

impl RemotePoolFixture {
    /// `start` prepares the provider without submitting definitions, so a test
    /// can interrupt registration during the first startup attempt.
    pub(in crate::tests) async fn start(database: PgPool) -> eyre::Result<Self> {
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
                    logical_pool: REMOTE_POOL.into(),
                },
                v1::ValueDomain::Integer,
                [20_001_i64, 20_002, 20_003, 50_001, 50_002]
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                None,
            )?,
            releases: Arc::default(),
            describes: Arc::default(),
            defines: Arc::default(),
            define_requests: Arc::default(),
            describe_response: Arc::default(),
            stats_unavailable: Arc::default(),
            lose_define_reply: Arc::default(),
            reject_define_call: Arc::default(),
            define_pause: Arc::default(),
            release_pause: Arc::default(),
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

        // Keep the API fixture's Integrated pools and add independent test pools.
        let mut definitions = db::resource_pool::all_stored_defs(&env.pool).await?;
        for (name, start, auto_assign) in [
            (REMOTE_POOL, 20_001, true),
            (SECOND_REMOTE_POOL, 50_001, false),
        ] {
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
        let selections = [REMOTE_POOL, SECOND_REMOTE_POOL]
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
                        // The alias differs from the provider ID returned by Describe.
                        remote_pool: REMOTE_POOL.into(),
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

    /// `new` accepts both definitions before a test allocates from either pool.
    pub(in crate::tests) async fn new(database: PgPool) -> eyre::Result<Self> {
        let mut fixture = Self::start(database).await?;
        fixture
            .initialize(PoolDefinitionSource::SeedConfiguration)
            .await?;
        Ok(fixture)
    }

    /// `initialize` rebuilds the API over the same database and provider.
    /// The returned logs let tests check the seed reconciler's drift diagnostics.
    /// Recovery stays stopped so tests can inspect queued release requests
    /// before starting the worker that contacts the provider.
    pub(in crate::tests) async fn initialize(
        &mut self,
        source: PoolDefinitionSource,
    ) -> eyre::Result<Vec<CapturedLog>> {
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

    /// `remote` reads the coordinator installed in the API without constructing a
    /// second allocation path for the test.
    pub(in crate::tests) fn remote(&self, name: &str) -> &RemoteResourcePool {
        self.env
            .api
            .resource_pool_registry
            .get(name)
            .expect("registered test pool")
            .backend
            .remote()
            .expect("remote test pool")
    }

    /// `stop` joins the local provider before its database fixture is torn down.
    pub(in crate::tests) async fn stop(mut self) -> eyre::Result<()> {
        self.cancel.cancel();
        while let Some(result) =
            tokio::time::timeout(Duration::from_secs(5), self.server.join_next()).await?
        {
            result??;
        }
        Ok(())
    }
}

/// `ObservedProvider` injects registration and inspection failures around the
/// local provider. Describe, Define, and Release counters record incoming calls,
/// including calls that are rejected or paused.
#[derive(Clone)]
pub(in crate::tests) struct ObservedProvider {
    pub(in crate::tests) local: LocalProvider,
    pub(in crate::tests) releases: Arc<AtomicUsize>,
    pub(in crate::tests) describes: Arc<AtomicUsize>,
    pub(in crate::tests) defines: Arc<AtomicUsize>,
    pub(in crate::tests) define_requests: Arc<Mutex<Vec<v1::DefineRequest>>>,
    /// Replaces the whole successful Describe response until cleared.
    pub(in crate::tests) describe_response: Arc<Mutex<Option<v1::DescribeResponse>>>,
    pub(in crate::tests) stats_unavailable: Arc<AtomicBool>,
    pub(in crate::tests) lose_define_reply: Arc<AtomicBool>,
    /// Rejects this one-based Define call number; zero disables rejection.
    pub(in crate::tests) reject_define_call: Arc<AtomicUsize>,
    /// Pauses after the local provider has accepted Define.
    pub(in crate::tests) define_pause: Arc<PausedCall>,
    /// Pauses before the local provider releases the allocation.
    pub(in crate::tests) release_pause: Arc<PausedCall>,
}

/// `PausedCall` signals entry and holds one RPC until its caller drops it.
/// Create a new instance for another pause because `dropped` stays cancelled.
#[derive(Default)]
pub(in crate::tests) struct PausedCall {
    pub(in crate::tests) pause_next: AtomicBool,
    pub(in crate::tests) entered: Notify,
    pub(in crate::tests) dropped: CancellationToken,
}

impl PausedCall {
    /// `wait` pauses at most one call, so a restarted caller can retry normally.
    async fn wait(&self) {
        if self.pause_next.swap(false, Ordering::SeqCst) {
            let _dropped = self.dropped.clone().drop_guard();
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

#[tonic::async_trait]
impl ResourcePool for ObservedProvider {
    async fn describe(
        &self,
        request: Request<v1::DescribeRequest>,
    ) -> Result<Response<v1::DescribeResponse>, Status> {
        self.describes.fetch_add(1, Ordering::SeqCst);
        let mut response = self.local.describe(request).await?;
        // An alias can still resolve while identifying a different provider pool.
        if let Some(replacement) = self
            .describe_response
            .lock()
            .expect("Describe response")
            .as_ref()
        {
            *response.get_mut() = replacement.clone();
        }
        Ok(response)
    }

    async fn define(
        &self,
        request: Request<v1::DefineRequest>,
    ) -> Result<Response<v1::DefineResponse>, Status> {
        let call = self.defines.fetch_add(1, Ordering::SeqCst) + 1;
        self.define_requests
            .lock()
            .expect("Define requests")
            .push(request.get_ref().clone());
        if self.reject_define_call.load(Ordering::SeqCst) == call {
            return Err(Status::permission_denied("injected registration rejection"));
        }
        let lose_reply = self.lose_define_reply.load(Ordering::SeqCst)
            && request
                .get_ref()
                .pool
                .as_ref()
                .is_some_and(|pool| pool.logical_pool == REMOTE_POOL);
        let response = self.local.define(request).await?;
        self.define_pause.wait().await;
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
        self.local.allocate(request).await
    }

    async fn allocate_exact(
        &self,
        request: Request<v1::AllocateExactRequest>,
    ) -> Result<Response<v1::AllocateExactResponse>, Status> {
        self.local.allocate_exact(request).await
    }

    async fn recover(
        &self,
        request: Request<v1::RecoverRequest>,
    ) -> Result<Response<v1::RecoverResponse>, Status> {
        self.local.recover(request).await
    }

    async fn cancel(
        &self,
        request: Request<v1::CancelRequest>,
    ) -> Result<Response<v1::CancelResponse>, Status> {
        self.local.cancel(request).await
    }

    async fn release(
        &self,
        request: Request<v1::ReleaseRequest>,
    ) -> Result<Response<v1::ReleaseResponse>, Status> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        self.release_pause.wait().await;
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
