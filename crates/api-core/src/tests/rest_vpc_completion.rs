// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! One cross-service proof of REST completion after Core commits without a reply.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use carbide_resource_pool::PoolDefinitionSource;
use carbide_uuid::vpc::VpcId;
use eyre::WrapErr;
use model::resource_pool::OwnerType;
use model::resource_pool::allocation::AllocationState;
use model::resource_pool::common::VPC_VNI;
use rpc::forge::forge_server::{Forge, ForgeServer};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::body::Body;
use tonic::transport::Server;
use tonic::{Request, Status};
use tower::Service;

use crate::tests::common::resource_pool::{RemotePool, RemotePoolFixture};

/// Replaces the first CreateVpc response after the real handler returns.
/// Other methods remain available for the Site client and inventory recovery.
#[derive(Clone)]
struct LoseCreateReply<S> {
    inner: S,
    calls: Arc<AtomicUsize>,
}

impl<S> Service<http::Request<Body>> for LoseCreateReply<S>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = http::Response<Body>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let lose_reply = request.uri().path() == "/forge.Forge/CreateVpc"
            && self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        // Keep the service whose readiness was checked for this request.
        let replacement = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, replacement);
        Box::pin(async move {
            let response = inner.call(request).await?;
            if lose_reply {
                return Ok(Status::unavailable("test lost the committed create reply").into_http());
            }
            Ok(response)
        })
    }
}

/// Keeps Core and its provider real while Go exercises its actual REST handler,
/// Site workflow/activity, durable Temporal history, and reconstructed recovery.
/// Run `make core/tests TEST_ARGS='-p carbide-api-core rest_vpc_completion -- --ignored'`.
/// The helper supplies private PostgreSQL, DATABASE_URL, CORE_TEST_POSTGRES_CONTAINER
/// and REPO_ROOT; Go and the Temporal dev-server download must be available.
/// Ordinary suites do not select this test.
#[test]
#[ignore = "cross-service proof; requires the isolated Core test helper and Go"]
fn rest_vpc_completion_after_lost_reply() -> eyre::Result<()> {
    let args = sqlx::testing::TestArgs::new(concat!(
        module_path!(),
        "::rest_vpc_completion_after_lost_reply"
    ));
    let test_fn: fn(PgPool) -> _ = rest_vpc_completion;
    // Match Core's worker stack: the generated gRPC dispatcher exceeds libtest's default.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || sqlx_testing::TestFn::run_test(test_fn, args))
        .unwrap()
        .join()
        .expect("REST completion test panicked")
}

/// Serves Core while the Go test loses a create reply and recovers the original VPC.
async fn rest_vpc_completion(database: PgPool) -> eyre::Result<()> {
    let database_url = url::Url::parse(
        &std::env::var("DATABASE_URL")
            .wrap_err("DATABASE_URL is required by the isolated test helper")?,
    )?;
    let port = database_url
        .port()
        .ok_or_else(|| eyre::eyre!("private PostgreSQL port missing"))?;
    let container = std::env::var("CORE_TEST_POSTGRES_CONTAINER")
        .wrap_err("CORE_TEST_POSTGRES_CONTAINER is required by the isolated test helper")?;
    let mut fixture = RemotePoolFixture::start_with(
        database,
        &[RemotePool {
            name: VPC_VNI,
            start: 20_001,
            auto_assign: true,
        }],
        None,
    )
    .await?;
    fixture
        .initialize(PoolDefinitionSource::SeedConfiguration)
        .await?;
    fixture
        .env
        .api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "rest-completion-tenant".into(),
            routing_profile_type: Some("INTERNAL".into()),
            metadata: Some(rpc::Metadata {
                name: "REST completion tenant".into(),
                ..Default::default()
            }),
        }))
        .await?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let layer = tower::layer::layer_fn(move |inner| LoseCreateReply {
        inner,
        calls: counter.clone(),
    });
    let cancel = fixture.cancel.child_token();
    let shutdown = cancel.clone();
    let api = fixture.env.api.clone();
    let server = tokio::spawn(async move {
        Server::builder()
            .layer(layer)
            .add_service(ForgeServer::from_arc(api))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown.cancelled())
            .await
    });

    // Reuse this invocation's private PostgreSQL container. Go's fixture uses
    // its separate nicotest database, leaving Core's SQLx database unchanged.
    let vpc_id: VpcId = uuid::Uuid::new_v4().to_string().parse()?;
    let result = async {
        let mut child = Command::new("make")
            .current_dir(std::env::var("REPO_ROOT").wrap_err("REPO_ROOT is required by the isolated test helper")?)
            .arg("rest-api/test-api")
            .arg(format!("POSTGRES_CONTAINER_NAME={container}"))
            .arg(format!("POSTGRES_PORT={port}"))
            .arg("POSTGRES_DB=postgres")
            .arg("TEST_PACKAGES=./pkg/api/handler -run=TestCreateVPCHandler_Handle/live_Core_completion")
            .env("PGPORT", port.to_string())
            // This helper uses its private host port, including when launched by CI.
            .env("CI", "false")
            .env("NICO_TEST_CORE_ENDPOINT", address.to_string())
            .env("NICO_TEST_VPC_ID", vpc_id.to_string())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let group = child.id().expect("spawned Go proof process group");
        match tokio::time::timeout(Duration::from_secs(300), child.wait()).await {
            Ok(status) => Ok(status?),
            Err(_) => {
                // Kill the owned Make/Go/Temporal descendants as well as Make itself.
                let group_name = format!("-{group}");
                let kill = tokio::time::timeout(Duration::from_secs(5), Command::new("kill")
                    .args(["-KILL", "--", &group_name]).kill_on_drop(true).status()).await;
                let joined = tokio::time::timeout(Duration::from_secs(5), child.kill()).await;
                eyre::bail!("REST proof timed out; group cleanup: {kill:?}; child join: {joined:?}");
            }
        }
    }.await;
    cancel.cancel();
    let server_result = server
        .await
        .wrap_err("join REST completion test server")
        .and_then(|result| result.wrap_err("serve REST completion test requests"));
    if let Err(error) = server_result {
        let cleanup = fixture.stop().await;
        return match cleanup {
            Ok(()) => Err(error),
            Err(cleanup_error) => {
                Err(error.wrap_err(format!("fixture cleanup also failed: {cleanup_error}")))
            }
        };
    }
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            fixture.stop().await?;
            return Err(error);
        }
    };
    if !status.success() {
        fixture.stop().await?;
        eyre::bail!("REST completion proof failed: {status}");
    }

    // The lost-reply case and the separate successful create account for two calls.
    // Recovery must not add another, even if Core's existing-ID check would reject it.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.provider.allocations.load(Ordering::SeqCst), 2);
    let saved = db::resource_pool::remote::latest_for_owner_slot(
        &fixture.env.pool,
        OwnerType::Vpc,
        &vpc_id.to_string(),
        "vni",
    )
    .await?
    .expect("original VPC assignment");
    assert_eq!(saved.state, AllocationState::Attached);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_pool_allocations
         WHERE owner_id = $1 AND remote_request->>'slot' = 'vni'",
    )
    .bind(vpc_id.to_string())
    .fetch_one(&fixture.env.pool)
    .await?;
    assert_eq!(count, 1);
    let attached: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_pool_allocations
         WHERE remote_request->>'slot' = 'vni' AND state = 'attached'",
    )
    .fetch_one(&fixture.env.pool)
    .await?;
    assert_eq!(attached, 2);
    fixture.stop().await
}
