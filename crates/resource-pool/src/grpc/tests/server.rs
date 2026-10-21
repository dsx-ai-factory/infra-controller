// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::convert::Infallible;
use std::error::Error;
use std::future::Future;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use carbide_resource_pool_api::testing::LocalProvider;
use carbide_resource_pool_api::v1::eligibility::Entries;
use carbide_resource_pool_api::v1::operation_status::State;
use carbide_resource_pool_api::v1::resource_pool_server::{ResourcePool, ResourcePoolServer};
use carbide_resource_pool_api::v1::{
    AllocateExactRequest, AllocateExactResponse, AllocateRequest, AllocateResponse, Allocation,
    AllocationOperation, AssignmentMode, CancelRequest, CancelResponse, DefineRequest,
    DefineResponse, DescribeRequest, DescribeResponse, Eligibility, Empty, LookupRequest,
    LookupResponse, Owner, PoolBinding, RecoverRequest, RecoverResponse, ReleaseRequest,
    ReleaseResponse, RouteTarget, RouteTargetMode, RouteTargetPolicy, RouteTargets, StatsRequest,
    StatsResponse, ValueDomain, ValueRange,
};
use http_body::Frame;
use http_body_util::BodyExt;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::body::Body;
use tonic::server::NamedService;
use tonic::transport::{Server, ServerTlsConfig};
use tonic::{Code, Request, Response, Status};
use tower::Service;
use url::Url;

pub(in crate::grpc) const TEST_TIMEOUT: Duration = Duration::from_secs(5);

/// `TestServer` runs a provider on a loopback port and owns its server task.
/// Tests call `stop` to cancel and join that task before returning.
pub(in crate::grpc) struct TestServer {
    pub(in crate::grpc) endpoint: Url,
    pub(in crate::grpc) accepted_connections: Arc<AtomicUsize>,
    shutdown: CancellationToken,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl TestServer {
    pub(super) async fn start(provider: impl ResourcePool) -> Self {
        Self::start_service(ResourcePoolServer::new(provider), None).await
    }

    pub(in crate::grpc) async fn start_tls(
        provider: impl ResourcePool,
        tls: ServerTlsConfig,
    ) -> Self {
        Self::start_service(ResourcePoolServer::new(provider), Some(tls)).await
    }

    pub(in crate::grpc) async fn start_service<S, B>(
        service: S,
        tls: Option<ServerTlsConfig>,
    ) -> Self
    where
        S: Service<http::Request<Body>, Response = http::Response<B>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
        S::Future: Send + 'static,
        B: http_body::Body<Data = Bytes> + Send + 'static,
        B::Error: Into<Box<dyn Error + Send + Sync>>,
    {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind provider listener");
        Self::start_service_with_listener(listener, service, tls)
    }

    pub(in crate::grpc) fn start_service_with_listener<S, B>(
        listener: TcpListener,
        service: S,
        tls: Option<ServerTlsConfig>,
    ) -> Self
    where
        S: Service<http::Request<Body>, Response = http::Response<B>, Error = Infallible>
            + NamedService
            + Clone
            + Send
            + Sync
            + 'static,
        S::Future: Send + 'static,
        B: http_body::Body<Data = Bytes> + Send + 'static,
        B::Error: Into<Box<dyn Error + Send + Sync>>,
    {
        let address = listener.local_addr().expect("provider address");
        let scheme = if tls.is_some() { "https" } else { "http" };
        let endpoint = Url::parse(&format!("{scheme}://{address}")).expect("provider URL");
        let mut server = Server::builder();
        if let Some(tls) = tls {
            server = server.tls_config(tls).expect("configure server TLS");
        }
        let shutdown = CancellationToken::new();
        let accepted_connections = Arc::new(AtomicUsize::new(0));
        let accepted = accepted_connections.clone();
        let incoming = TcpListenerStream::new(listener).map(move |connection| {
            if connection.is_ok() {
                accepted.fetch_add(1, Ordering::SeqCst);
            }
            connection
        });
        let task = tokio::spawn(
            server
                .add_service(service)
                .serve_with_incoming_shutdown(incoming, shutdown.clone().cancelled_owned()),
        );
        Self {
            endpoint,
            accepted_connections,
            shutdown,
            task,
        }
    }

    pub(in crate::grpc) async fn stop(mut self) {
        self.shutdown.cancel();
        match tokio::time::timeout(TEST_TIMEOUT, &mut self.task).await {
            Ok(result) => result
                .expect("join provider server")
                .expect("serve provider"),
            Err(_) => {
                self.task.abort();
                assert!(
                    self.task
                        .await
                        .expect_err("abort stalled server")
                        .is_cancelled()
                );
                panic!("provider server did not stop within {TEST_TIMEOUT:?}");
            }
        }
    }
}

/// `Gate` signals a provider call's arrival through `entered`, then waits for
/// `release`. `dropped` signals that the wait completed or was cancelled.
#[derive(Default)]
pub(in crate::grpc) struct Gate {
    pub(super) calls: AtomicUsize,
    pub(in crate::grpc) entered: Notify,
    pub(in crate::grpc) release: Notify,
    pub(super) dropped: Arc<Notify>,
}

impl Gate {
    async fn wait(&self) {
        let _dropped = NotifyOnDrop(self.dropped.clone());
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
    }
}

struct NotifyOnDrop(Arc<Notify>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[derive(Clone)]
pub(in crate::grpc) enum Fault {
    None,
    WithholdAllocation(Arc<Gate>),
    AllocationUnavailable,
    DelayAllocation(Arc<Gate>),
    StallDescription(Arc<Gate>),
    MismatchedOperation,
    Malformed(MalformedResponse),
    Description(DescribeResponse),
    LookupUnimplemented,
    Stats(Code),
}

#[derive(Clone, Copy, Debug)]
pub(in crate::grpc) enum MalformedResponse {
    Describe,
    AllocateExact,
    Recover,
    Cancel,
    Lookup,
    Stats,
}

/// `FaultProvider` wraps the shared provider with one selected delivery or reply
/// fault. Its observations let tests distinguish provider work from client waiting.
#[derive(Clone)]
pub(in crate::grpc) struct FaultProvider {
    pub(super) provider: LocalProvider,
    fault: Fault,
    pub(super) allocation_calls: Arc<AtomicUsize>,
    pub(super) committed: Arc<Mutex<Option<Allocation>>>,
    pub(super) stats_calls: Arc<AtomicUsize>,
    pub(super) traceparent: Arc<Mutex<Option<String>>>,
    pub(super) grpc_timeout: Arc<Mutex<Option<String>>>,
    pub(super) describe_calls: Arc<AtomicUsize>,
}

impl FaultProvider {
    pub(in crate::grpc) fn new(provider: LocalProvider, fault: Fault) -> Self {
        Self {
            provider,
            fault,
            allocation_calls: Arc::default(),
            committed: Arc::default(),
            stats_calls: Arc::default(),
            traceparent: Arc::default(),
            grpc_timeout: Arc::default(),
            describe_calls: Arc::default(),
        }
    }
}

#[tonic::async_trait]
impl ResourcePool for FaultProvider {
    async fn describe(
        &self,
        request: Request<DescribeRequest>,
    ) -> Result<Response<DescribeResponse>, Status> {
        self.describe_calls.fetch_add(1, Ordering::SeqCst);
        *self.grpc_timeout.lock().expect("timeout header lock") = request
            .metadata()
            .get("grpc-timeout")
            .map(|value| value.to_str().expect("ASCII grpc-timeout").to_owned());
        *self.traceparent.lock().expect("trace header lock") = request
            .metadata()
            .get("traceparent")
            .map(|value| value.to_str().expect("ASCII traceparent").to_owned());
        if let Fault::StallDescription(gate) = &self.fault {
            gate.wait().await;
        }
        let mut response = self.provider.describe(request).await?;
        match &self.fault {
            Fault::Malformed(MalformedResponse::Describe) => {
                response.get_mut().authority_id.clear()
            }
            Fault::Description(description) => *response.get_mut() = description.clone(),
            _ => {}
        }
        Ok(response)
    }

    async fn define(
        &self,
        request: Request<DefineRequest>,
    ) -> Result<Response<DefineResponse>, Status> {
        self.provider.define(request).await
    }

    async fn allocate(
        &self,
        request: Request<AllocateRequest>,
    ) -> Result<Response<AllocateResponse>, Status> {
        self.allocation_calls.fetch_add(1, Ordering::SeqCst);
        if let Fault::DelayAllocation(gate) = &self.fault {
            gate.wait().await;
        }
        let mut response = self.provider.allocate(request).await?;
        *self.committed.lock().expect("committed allocation lock") =
            response.get_ref().allocation.clone();
        match &self.fault {
            Fault::WithholdAllocation(gate) => gate.wait().await,
            Fault::AllocationUnavailable => {
                return Err(Status::unavailable("allocation reply unavailable"));
            }
            Fault::MismatchedOperation => {
                response
                    .get_mut()
                    .allocation
                    .as_mut()
                    .expect("provider allocation")
                    .operation
                    .as_mut()
                    .expect("provider operation")
                    .request_id = "another-request".into();
            }
            _ => {}
        }
        Ok(response)
    }

    async fn allocate_exact(
        &self,
        request: Request<AllocateExactRequest>,
    ) -> Result<Response<AllocateExactResponse>, Status> {
        let mut response = self.provider.allocate_exact(request).await?;
        if matches!(
            self.fault,
            Fault::Malformed(MalformedResponse::AllocateExact)
        ) {
            response
                .get_mut()
                .allocation
                .as_mut()
                .expect("exact allocation")
                .value = Some(2.into());
        }
        Ok(response)
    }

    async fn recover(
        &self,
        request: Request<RecoverRequest>,
    ) -> Result<Response<RecoverResponse>, Status> {
        let mut response = self.provider.recover(request).await?;
        if matches!(self.fault, Fault::Malformed(MalformedResponse::Recover)) {
            response
                .get_mut()
                .operation_identity
                .as_mut()
                .expect("recovered identity")
                .request_id = "another-request".into();
        }
        Ok(response)
    }

    async fn cancel(
        &self,
        request: Request<CancelRequest>,
    ) -> Result<Response<CancelResponse>, Status> {
        let mut response = self.provider.cancel(request).await?;
        if matches!(self.fault, Fault::Malformed(MalformedResponse::Cancel)) {
            response
                .get_mut()
                .operation_status
                .as_mut()
                .expect("cancel status")
                .state = Some(State::Unknown(Empty {}));
        }
        Ok(response)
    }

    async fn lookup(
        &self,
        request: Request<LookupRequest>,
    ) -> Result<Response<LookupResponse>, Status> {
        if matches!(self.fault, Fault::LookupUnimplemented) {
            return Err(Status::unimplemented("lookup is unsupported"));
        }
        let mut response = self.provider.lookup(request).await?;
        if matches!(self.fault, Fault::Malformed(MalformedResponse::Lookup)) {
            response
                .get_mut()
                .allocation
                .as_mut()
                .expect("looked up allocation")
                .allocation_id = "another-allocation".into();
        }
        Ok(response)
    }

    async fn release(
        &self,
        request: Request<ReleaseRequest>,
    ) -> Result<Response<ReleaseResponse>, Status> {
        self.provider.release(request).await
    }

    async fn stats(
        &self,
        request: Request<StatsRequest>,
    ) -> Result<Response<StatsResponse>, Status> {
        self.stats_calls.fetch_add(1, Ordering::SeqCst);
        if let Fault::Stats(code) = self.fault {
            return Err(Status::new(code, "injected provider status"));
        }
        let mut response = self.provider.stats(request).await?;
        if matches!(self.fault, Fault::Malformed(MalformedResponse::Stats)) {
            response.get_mut().stats = None;
        }
        Ok(response)
    }
}

// Headers are sent immediately, but the body never completes. A timeout
// around channel acquisition or response headers cannot bound this RPC.
#[derive(Clone)]
pub(super) struct StalledBodyService(pub(super) Arc<Gate>);

impl NamedService for StalledBodyService {
    const NAME: &'static str = <ResourcePoolServer<LocalProvider> as NamedService>::NAME;
}

impl Service<http::Request<Body>> for StalledBodyService {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let gate = self.0.clone();
        gate.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            request
                .into_body()
                .collect()
                .await
                .expect("read unary request");
            Ok(http::Response::builder()
                .header("content-type", "application/grpc")
                .body(Body::new(StalledBody {
                    _dropped: NotifyOnDrop(gate.dropped.clone()),
                    gate,
                }))
                .expect("build stalled gRPC response"))
        })
    }
}

struct StalledBody {
    gate: Arc<Gate>,
    _dropped: NotifyOnDrop,
}

impl http_body::Body for StalledBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        self.gate.entered.notify_one();
        Poll::Pending
    }
}

pub(in crate::grpc) async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, future)
        .await
        .expect("provider test operation completed within five seconds")
}

pub(in crate::grpc) fn pool() -> PoolBinding {
    PoolBinding {
        site_id: "test-site".into(),
        authority_id: "test-provider".into(),
        pool_id: "provider-vnis".into(),
        binding_id: "vpc-binding".into(),
        logical_pool: "vpc-vni".into(),
    }
}

pub(in crate::grpc) fn provider() -> LocalProvider {
    LocalProvider::new(
        pool(),
        ValueDomain::Integer,
        (1..=5).map(Into::into).collect(),
        Some(route_target_policy()),
    )
    .expect("create provider inventory")
}

pub(super) fn definition() -> DefineRequest {
    DefineRequest {
        pool: Some(pool()),
        eligibility_revision: "initial".into(),
        previous_revision: None,
        value_domain: ValueDomain::Integer as i32,
        eligibility: vec![Eligibility {
            assignment_mode: AssignmentMode::Automatic as i32,
            entries: Some(Entries::Range(ValueRange {
                start: Some(1.into()),
                end: Some(6.into()),
            })),
        }],
    }
}

pub(super) fn operation(request_id: &str) -> AllocationOperation {
    AllocationOperation {
        pool: Some(pool()),
        request_id: request_id.into(),
        eligibility_revision: "initial".into(),
        owner: Some(Owner {
            owner_type: "vpc".into(),
            owner_id: "test-vpc".into(),
            slot: "vni".into(),
            generation: 0,
        }),
    }
}

pub(super) fn route_target_policy() -> RouteTargetPolicy {
    let target = RouteTarget { asn: 64512, vni: 1 };
    RouteTargetPolicy {
        mode: RouteTargetMode::ReplaceGenerated as i32,
        imports: Some(RouteTargets { values: vec![] }),
        exports: Some(RouteTargets {
            values: vec![target, target],
        }),
        peer_identity: vec![target],
    }
}
