// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::convert::Infallible;
use std::error::Error;
use std::future::Future;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    DefineResponse, DescribeRequest, DescribeResponse, Eligibility, Empty, Failure, FailureCode,
    LookupRequest, LookupResponse, OperationStatus, Owner, PoolBinding, RecoverRequest,
    RecoverResponse, ReleaseRequest, ReleaseResponse, RouteTarget, RouteTargetMode,
    RouteTargetPolicy, RouteTargets, StatsRequest, StatsResponse, ValueDomain, ValueRange,
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
pub(crate) struct TestServer {
    pub(crate) endpoint: Url,
    pub(in crate::grpc) accepted_connections: Arc<AtomicUsize>,
    shutdown: CancellationToken,
    task: JoinHandle<Result<(), tonic::transport::Error>>,
}

impl TestServer {
    pub(crate) async fn start(provider: impl ResourcePool) -> Self {
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

    pub(crate) async fn stop(mut self) {
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
/// For delayed allocation, `allocation_finished` signals handler completion or
/// cancellation, including any provider work after the gate opens.
#[derive(Default)]
pub(crate) struct Gate {
    pub(crate) calls: AtomicUsize,
    pub(crate) entered: Notify,
    pub(crate) release: Notify,
    pub(crate) dropped: Arc<Notify>,
    pub(crate) allocation_finished: Arc<Notify>,
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
pub(crate) enum Fault {
    None,
    // Wait after allocation commits, before delivering the reply.
    WithholdAllocation(Arc<Gate>),
    AllocationUnavailable,
    AllocationInternal,
    RecoverUnavailableOnce,
    RecoverStatus(Code),
    RecoverUnknownOnce,
    RecoverNotFoundOnce,
    StallRecovery(Arc<Gate>),
    // Keep one request blocked across repeated scanner passes.
    StallRecoveryForRequest {
        request_id: String,
        gate: Arc<Gate>,
    },
    RecoveredOwnerMismatch,
    RecoveredPolicyMismatchOnce,
    RecoveredWrongDomain,
    RecoveredIneligibleAllocation,
    RecoveredFailureWithNul,
    // Fail the first `Cancel` before cleanup; the flag completes later pending
    // cleanup immediately before `Recover` reads its status.
    CancelUnavailableOnce {
        complete_before_recover: Arc<AtomicBool>,
    },
    // Lose the first reply after the provider has completed `Cancel`.
    CancelReplyUnavailableOnce,
    // Fail before the provider releases the reservation.
    ReleaseUnavailableOnce,
    // Lose the first reply after the provider has released the reservation.
    ReleaseReplyUnavailableOnce,
    ReleaseNotAppliedOnce,
    // Wait before allocation so a terminal `Cancel` can arrive first.
    DelayAllocation(Arc<Gate>),
    StallDescription(Arc<Gate>),
    MismatchedOperation,
    IneligibleAllocation,
    Malformed(MalformedResponse),
    Description(DescribeResponse),
    LookupUnimplemented,
    Stats(Code),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum MalformedResponse {
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
pub(crate) struct FaultProvider {
    pub(crate) provider: LocalProvider,
    fault: Fault,
    /// Counts both `Allocate` and `AllocateExact` requests.
    pub(crate) allocation_calls: Arc<AtomicUsize>,
    pub(crate) recover_calls: Arc<AtomicUsize>,
    pub(crate) cancel_calls: Arc<AtomicUsize>,
    pub(crate) release_calls: Arc<AtomicUsize>,
    pub(crate) committed: Arc<Mutex<Option<Allocation>>>,
    pub(super) stats_calls: Arc<AtomicUsize>,
    pub(super) traceparent: Arc<Mutex<Option<String>>>,
    pub(super) grpc_timeout: Arc<Mutex<Option<String>>>,
    pub(super) describe_calls: Arc<AtomicUsize>,
}

impl FaultProvider {
    pub(crate) fn new(provider: LocalProvider, fault: Fault) -> Self {
        Self {
            provider,
            fault,
            allocation_calls: Arc::default(),
            recover_calls: Arc::default(),
            cancel_calls: Arc::default(),
            release_calls: Arc::default(),
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
        let first_call = self.allocation_calls.fetch_add(1, Ordering::SeqCst) == 0;
        let _finished = if first_call && let Fault::DelayAllocation(gate) = &self.fault {
            let finished = NotifyOnDrop(gate.allocation_finished.clone());
            gate.wait().await;
            Some(finished)
        } else {
            None
        };
        let mut response = self.provider.allocate(request).await?;
        *self.committed.lock().expect("committed allocation lock") =
            response.get_ref().allocation.clone();
        match &self.fault {
            Fault::WithholdAllocation(gate) => gate.wait().await,
            Fault::AllocationUnavailable => {
                return Err(Status::unavailable("allocation reply unavailable"));
            }
            Fault::AllocationInternal => {
                return Err(Status::internal("allocation reply interrupted"));
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
            Fault::IneligibleAllocation => {
                response
                    .get_mut()
                    .allocation
                    .as_mut()
                    .expect("provider allocation")
                    .value = Some(99.into());
            }
            _ => {}
        }
        Ok(response)
    }

    async fn allocate_exact(
        &self,
        request: Request<AllocateExactRequest>,
    ) -> Result<Response<AllocateExactResponse>, Status> {
        self.allocation_calls.fetch_add(1, Ordering::SeqCst);
        let mut response = self.provider.allocate_exact(request).await?;
        *self.committed.lock().expect("committed allocation lock") =
            response.get_ref().allocation.clone();
        if let Fault::WithholdAllocation(gate) = &self.fault {
            gate.wait().await;
        }
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
        let first_call = self.recover_calls.fetch_add(1, Ordering::SeqCst) == 0;
        match &self.fault {
            Fault::StallRecovery(gate) => gate.wait().await,
            Fault::StallRecoveryForRequest { request_id, gate }
                if request
                    .get_ref()
                    .operation_identity
                    .as_ref()
                    .is_some_and(|identity| &identity.request_id == request_id) =>
            {
                gate.wait().await;
            }
            _ => {}
        }
        match self.fault {
            Fault::RecoverStatus(code) => {
                return Err(Status::new(code, "injected recovery status"));
            }
            Fault::RecoverUnavailableOnce if first_call => {
                return Err(Status::unavailable("recovery unavailable"));
            }
            Fault::RecoverUnknownOnce if first_call => {
                return Ok(Response::new(RecoverResponse {
                    operation_identity: request.into_inner().operation_identity,
                    operation_status: Some(OperationStatus {
                        state: Some(State::Unknown(Empty {})),
                    }),
                }));
            }
            Fault::RecoverNotFoundOnce if first_call => {
                return Err(Status::not_found("recovery not found"));
            }
            _ => {}
        }
        if let Fault::CancelUnavailableOnce {
            complete_before_recover,
        } = &self.fault
            && complete_before_recover.swap(false, Ordering::SeqCst)
        {
            // Finish the provider's actual cleanup after its `Pending` reply,
            // so this `Recover` reports the resulting terminal state.
            self.provider.complete_cleanup(
                request
                    .get_ref()
                    .operation_identity
                    .as_ref()
                    .expect("recovery identity"),
            )?;
        }
        let mut response = self.provider.recover(request).await?;
        if matches!(self.fault, Fault::RecoveredFailureWithNul) {
            response.get_mut().operation_status = Some(OperationStatus {
                state: Some(State::Failed(Failure {
                    code: FailureCode::FailedPrecondition.into(),
                    message: "provider\0diagnostic".into(),
                })),
            });
        }
        if matches!(self.fault, Fault::Malformed(MalformedResponse::Recover)) {
            response
                .get_mut()
                .operation_identity
                .as_mut()
                .expect("recovered identity")
                .request_id = "another-request".into();
        }
        if let Some(State::Allocated(allocation)) = response
            .get_mut()
            .operation_status
            .as_mut()
            .and_then(|status| status.state.as_mut())
        {
            match self.fault {
                Fault::RecoveredOwnerMismatch => {
                    allocation
                        .operation
                        .as_mut()
                        .expect("recovered allocation operation")
                        .owner
                        .as_mut()
                        .expect("recovered allocation owner")
                        .owner_id = "another-owner".into();
                }
                Fault::RecoveredPolicyMismatchOnce if first_call => {
                    allocation
                        .route_target_policy
                        .as_mut()
                        .expect("recovered allocation policy")
                        .peer_identity
                        .clear();
                }
                Fault::RecoveredWrongDomain => {
                    allocation.value = Some(Ipv4Addr::new(192, 0, 2, 1).into());
                }
                Fault::RecoveredIneligibleAllocation => {
                    allocation.value = Some(99.into());
                }
                _ => {}
            }
        }
        Ok(response)
    }

    async fn cancel(
        &self,
        request: Request<CancelRequest>,
    ) -> Result<Response<CancelResponse>, Status> {
        let first_call = self.cancel_calls.fetch_add(1, Ordering::SeqCst) == 0;
        if first_call && matches!(self.fault, Fault::CancelUnavailableOnce { .. }) {
            return Err(Status::unavailable("cancellation unavailable"));
        }
        let mut response = self.provider.cancel(request).await?;
        if first_call && matches!(self.fault, Fault::CancelReplyUnavailableOnce) {
            return Err(Status::unavailable("cancellation reply unavailable"));
        }
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
        let first_call = self.release_calls.fetch_add(1, Ordering::SeqCst) == 0;
        if first_call && matches!(self.fault, Fault::ReleaseUnavailableOnce) {
            return Err(Status::unavailable("release unavailable"));
        }
        if first_call && matches!(self.fault, Fault::ReleaseNotAppliedOnce) {
            return Ok(Response::new(ReleaseResponse { applied: false }));
        }
        let response = self.provider.release(request).await?;
        if first_call && matches!(self.fault, Fault::ReleaseReplyUnavailableOnce) {
            return Err(Status::unavailable("release reply unavailable"));
        }
        Ok(response)
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

pub(crate) async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, future)
        .await
        .expect("provider test operation completed within five seconds")
}

pub(crate) fn pool() -> PoolBinding {
    PoolBinding {
        site_id: "test-site".into(),
        authority_id: "test-provider".into(),
        pool_id: "provider-vnis".into(),
        binding_id: "vpc-binding".into(),
        logical_pool: "vpc-vni".into(),
    }
}

pub(crate) fn provider() -> LocalProvider {
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
