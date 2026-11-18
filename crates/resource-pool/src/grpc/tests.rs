// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

pub(crate) mod server;

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use carbide_resource_pool_api::testing::{LocalProvider, verify_provider};
use carbide_resource_pool_api::v1::operation_status::State;
use carbide_resource_pool_api::v1::resource_pool_server::{ResourcePool, ResourcePoolServer};
use carbide_resource_pool_api::v1::{
    AllocateExactRequest, AllocateExactResponse, AllocateRequest, AllocateResponse, CancelRequest,
    CancelResponse, DefineRequest, DefineResponse, DescribeRequest, DescribeResponse, Empty,
    LookupRequest, LookupResponse, OperationIdentity, PoolBinding, PoolStats, RecoverRequest,
    RecoverResponse, ReleaseRequest, ReleaseResponse, StatsRequest, StatsResponse, ValueDomain,
};
use opentelemetry::trace::{TraceContextExt, TracerProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use prost::Message;
use server::{
    Fault, FaultProvider, Gate, MalformedResponse, StalledBodyService, TEST_TIMEOUT, TestServer,
    bounded, definition, operation, pool, provider, route_target_policy,
};
use tokio::net::TcpSocket;
use tonic::{Code, Request, Response, Status};
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::prelude::*;

use super::{GrpcResourcePoolBackend, GrpcResourcePoolError, RpcError};

const STALL_TIMEOUT: Duration = Duration::from_millis(250);
const MESSAGE_LIMIT: usize = 4 * 1024 * 1024;

#[tokio::test]
async fn grpc_backend_satisfies_shared_provider_contract() {
    let provider = FaultProvider::new(provider(), Fault::None);
    let server = TestServer::start(provider.clone()).await;
    let client = backend(&server);
    bounded(verify_provider(
        &RemoteProvider(client.clone(), provider.allocation_calls.clone()),
        pool(),
        Some(route_target_policy()),
    ))
    .await;
    assert_eq!(server.accepted_connections.load(Ordering::SeqCst), 1);
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn refused_connection_returns_a_connect_error() {
    // Reserve the port without listening so another test cannot claim it.
    let socket = TcpSocket::new_v4().expect("create non-listening socket");
    socket
        .bind((Ipv4Addr::LOCALHOST, 0).into())
        .expect("reserve refused port");
    let address = socket.local_addr().expect("refused socket address");
    let client = GrpcResourcePoolBackend::new(
        url::Url::parse(&format!("http://{address}")).expect("refused endpoint URL"),
        None,
    )
    .expect("configure refused endpoint")
    .with_timeouts(STALL_TIMEOUT, TEST_TIMEOUT);
    assert!(matches!(
        bounded(client.describe(describe_request())).await,
        Err(GrpcResourcePoolError::Connect(_))
    ));
}

#[tokio::test]
async fn admission_limit_rejects_overflow_and_reuses_a_dropped_calls_slot() {
    let gate = Arc::new(Gate::default());
    let server = TestServer::start(FaultProvider::new(
        provider(),
        Fault::StallDescription(gate.clone()),
    ))
    .await;
    let client = backend(&server).with_timeouts(TEST_TIMEOUT, Duration::from_secs(30));
    let other_handle = client.clone();
    let mut calls = Vec::new();
    for _ in 0..32 {
        let mut call = Box::pin(client.describe(describe_request()));
        tokio::select! {
            result = &mut call => panic!("admitted call passed its gate: {result:?}"),
            () = bounded(gate.entered.notified()) => {}
        }
        calls.push(call);
    }
    assert!(matches!(
        bounded(other_handle.describe(describe_request())).await,
        Err(GrpcResourcePoolError::Busy)
    ));
    assert_eq!(gate.calls.load(Ordering::SeqCst), 32);

    drop(calls.pop().expect("one admitted call"));
    bounded(gate.dropped.notified()).await;
    let mut replacement = Box::pin(other_handle.describe(describe_request()));
    tokio::select! {
        result = &mut replacement => panic!("replacement call passed its gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    calls.push(replacement);
    assert_eq!(gate.calls.load(Ordering::SeqCst), 33);
    gate.release.notify_waiters();
    for call in calls {
        bounded(call).await.expect("complete admitted description");
    }
    drop(other_handle);
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn lost_allocation_reply_recovers_the_original_assignment_once() {
    enum ReplyFailure {
        Withheld(Arc<Gate>),
        Unavailable,
    }

    for failure in [
        ReplyFailure::Withheld(Arc::new(Gate::default())),
        ReplyFailure::Unavailable,
    ] {
        let fault = match &failure {
            ReplyFailure::Withheld(gate) => Fault::WithholdAllocation(gate.clone()),
            ReplyFailure::Unavailable => Fault::AllocationUnavailable,
        };
        let provider = seeded_provider(fault).await;
        let server = TestServer::start(provider.clone()).await;
        let client = backend(&server).with_timeouts(TEST_TIMEOUT, STALL_TIMEOUT);
        let request = allocation_request("lost-reply");

        match failure {
            ReplyFailure::Withheld(gate) => {
                let started = Instant::now();
                let mut allocation = Box::pin(client.allocate(request.clone()));
                tokio::select! {
                    result = &mut allocation => panic!("allocation finished before its reply was withheld: {result:?}"),
                    () = bounded(gate.entered.notified()) => {}
                }
                let error = bounded(allocation)
                    .await
                    .expect_err("allocation reply is withheld");
                assert_deadline_elapsed(started.elapsed());
                assert_header_deadline(error);
                bounded(gate.dropped.notified()).await;
            }
            ReplyFailure::Unavailable => assert_rpc_code(
                bounded(client.allocate(request.clone()))
                    .await
                    .expect_err("allocation reply is unavailable"),
                Code::Unavailable,
            ),
        }
        let committed = provider
            .committed
            .lock()
            .expect("committed allocation lock")
            .clone()
            .expect("allocation committed before its reply failed");
        assert_eq!(committed.operation, request.operation);
        assert_eq!(committed.route_target_policy, Some(route_target_policy()));
        assert_eq!(
            recover(&client, "lost-reply").await,
            State::Allocated(committed)
        );
        assert_eq!(provider.allocation_calls.load(Ordering::SeqCst), 1);
        assert_eq!(stats(&client).await.used, 1);

        drop(client);
        server.stop().await;
    }
}

#[tokio::test]
async fn terminal_cancel_prevents_an_allocation_already_in_flight() {
    let gate = Arc::new(Gate::default());
    let provider = seeded_provider(Fault::DelayAllocation(gate.clone())).await;
    let server = TestServer::start(provider.clone()).await;
    let client = backend(&server);
    let mut allocation = Box::pin(client.allocate(allocation_request("delayed")));
    tokio::select! {
        result = &mut allocation => panic!("allocation passed its gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }

    // The request reached the server but has not reached provider state. An
    // Unknown recovery is therefore insufficient evidence to abandon its ID.
    assert_eq!(recover(&client, "delayed").await, State::Unknown(Empty {}));
    let cancelled = client
        .cancel(CancelRequest {
            operation_identity: Some(identity("delayed")),
        })
        .await
        .expect("cancel delayed allocation");
    assert_eq!(
        cancelled
            .operation_status
            .expect("cancellation status")
            .state,
        Some(State::Cancelled(Empty {}))
    );

    gate.release.notify_one();
    assert_rpc_code(
        bounded(allocation)
            .await
            .expect_err("late allocation is rejected"),
        Code::FailedPrecondition,
    );
    assert_eq!(
        recover(&client, "delayed").await,
        State::Cancelled(Empty {})
    );
    assert_eq!(provider.allocation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(stats(&client).await.used, 0);

    // A different operation can use the capacity after terminal cancellation.
    let replacement = client
        .allocate_exact(AllocateExactRequest {
            operation: Some(operation("replacement")),
            value: Some(1.into()),
        })
        .await
        .expect("allocate capacity after cancellation");
    assert_eq!(
        replacement
            .allocation
            .expect("replacement allocation")
            .value,
        Some(1.into())
    );
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn dropping_a_call_closes_its_rpc_without_rolling_back_provider_work() {
    let gate = Arc::new(Gate::default());
    let provider = seeded_provider(Fault::WithholdAllocation(gate.clone())).await;
    let server = TestServer::start(provider.clone()).await;
    // Keep deadline expiry outside the five-second drop observation window.
    let client = backend(&server).with_timeouts(TEST_TIMEOUT, Duration::from_secs(30));
    let mut allocation = Box::pin(client.allocate(allocation_request("caller-dropped")));
    tokio::select! {
        result = &mut allocation => panic!("allocation reply escaped its gate: {result:?}"),
        () = bounded(gate.entered.notified()) => {}
    }
    drop(allocation);
    bounded(gate.dropped.notified()).await;

    let committed = provider
        .committed
        .lock()
        .expect("committed allocation lock")
        .clone()
        .expect("provider committed before caller dropped its future");
    assert_eq!(
        recover(&client, "caller-dropped").await,
        State::Allocated(committed)
    );
    assert_eq!(provider.allocation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(stats(&client).await.used, 1);
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn mismatched_success_is_rejected_and_original_operation_remains_recoverable() {
    let provider = seeded_provider(Fault::MismatchedOperation).await;
    let server = TestServer::start(provider.clone()).await;
    let client = backend(&server);
    assert!(matches!(
        client.allocate(allocation_request("malformed-reply")).await,
        Err(GrpcResourcePoolError::InvalidResponse(_))
    ));
    let committed = provider
        .committed
        .lock()
        .expect("committed allocation lock")
        .clone()
        .expect("provider assignment");
    assert_eq!(
        recover(&client, "malformed-reply").await,
        State::Allocated(committed)
    );
    assert_eq!(provider.allocation_calls.load(Ordering::SeqCst), 1);
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn public_operations_reject_malformed_successful_responses() {
    for malformed in [
        MalformedResponse::Describe,
        MalformedResponse::AllocateExact,
        MalformedResponse::Recover,
        MalformedResponse::Cancel,
        MalformedResponse::Lookup,
        MalformedResponse::Stats,
    ] {
        let provider = seeded_provider(Fault::Malformed(malformed)).await;
        let server = TestServer::start(provider).await;
        let client = backend(&server);
        let result = match malformed {
            MalformedResponse::Describe => client.describe(describe_request()).await.map(|_| ()),
            MalformedResponse::AllocateExact => client
                .allocate_exact(AllocateExactRequest {
                    operation: Some(operation("malformed-exact")),
                    value: Some(1.into()),
                })
                .await
                .map(|_| ()),
            MalformedResponse::Recover => client
                .recover(RecoverRequest {
                    operation_identity: Some(identity("malformed-recovery")),
                })
                .await
                .map(|_| ()),
            MalformedResponse::Cancel => client
                .cancel(CancelRequest {
                    operation_identity: Some(identity("malformed-cancel")),
                })
                .await
                .map(|_| ()),
            MalformedResponse::Lookup => {
                let allocation = client
                    .allocate(allocation_request("lookup-source"))
                    .await
                    .expect("create allocation for lookup")
                    .allocation
                    .expect("lookup allocation");
                client
                    .lookup(LookupRequest {
                        pool: Some(pool()),
                        allocation_id: allocation.allocation_id,
                    })
                    .await
                    .map(|_| ())
            }
            MalformedResponse::Stats => client.stats(stats_request()).await.map(|_| ()),
        };
        assert!(
            matches!(result, Err(GrpcResourcePoolError::InvalidResponse(_))),
            "{malformed:?}: {result:?}"
        );
        drop(client);
        server.stop().await;
    }
}

#[tokio::test]
async fn request_encoding_accepts_four_mib_and_rejects_one_more_byte() {
    let mut request = DescribeRequest {
        pool_alias: "p".repeat(MESSAGE_LIMIT),
    };
    let overhead = request.encoded_len() - MESSAGE_LIMIT;
    request.pool_alias.truncate(MESSAGE_LIMIT - overhead);
    assert_eq!(request.encoded_len(), MESSAGE_LIMIT);
    // Register the large alias so the provider accepts the boundary request.
    let provider = LocalProvider::new(
        PoolBinding {
            logical_pool: request.pool_alias.clone(),
            ..pool()
        },
        ValueDomain::Integer,
        Vec::new(),
        None,
    )
    .expect("create provider with a large alias");
    let provider = FaultProvider::new(provider, Fault::None);
    // A larger server limit isolates the client's encoding bound.
    let service =
        ResourcePoolServer::new(provider.clone()).max_decoding_message_size(2 * MESSAGE_LIMIT);
    let server = TestServer::start_service(service, None).await;
    let client = backend(&server);
    let response = bounded(client.describe(request.clone()))
        .await
        .expect("send exactly four MiB");
    assert_eq!(response.authority_id, pool().authority_id);
    assert_eq!(provider.describe_calls.load(Ordering::SeqCst), 1);

    request.pool_alias.push('p');
    assert_eq!(request.encoded_len(), MESSAGE_LIMIT + 1);
    // Hyper reports an outbound body error as an INTERNAL_ERROR stream reset.
    assert_rpc_code(
        bounded(client.describe(request))
            .await
            .expect_err("reject oversized request"),
        Code::Internal,
    );
    assert_eq!(provider.describe_calls.load(Ordering::SeqCst), 1);
    bounded(client.describe(DescribeRequest {
        pool_alias: pool().pool_id,
    }))
    .await
    .expect("reuse client after encoding rejection");
    assert_eq!(provider.describe_calls.load(Ordering::SeqCst), 2);
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn response_decoding_accepts_four_mib_and_rejects_one_more_byte() {
    for size in [MESSAGE_LIMIT, MESSAGE_LIMIT + 1] {
        let mut response = DescribeResponse {
            authority_id: "a".repeat(size),
            pool_id: pool().pool_id,
            value_domain: carbide_resource_pool_api::v1::ValueDomain::Integer as i32,
            supports_stats: true,
        };
        let overhead = response.encoded_len() - size;
        response.authority_id.truncate(size - overhead);
        assert_eq!(response.encoded_len(), size);
        let provider = FaultProvider::new(provider(), Fault::Description(response));
        // A larger server limit isolates the client's decoding bound.
        let service =
            ResourcePoolServer::new(provider.clone()).max_encoding_message_size(2 * MESSAGE_LIMIT);
        let server = TestServer::start_service(service, None).await;
        let client = backend(&server);
        let result = bounded(client.describe(describe_request())).await;
        if size == MESSAGE_LIMIT {
            assert_eq!(
                result.expect("receive exactly four MiB").encoded_len(),
                size
            );
        } else {
            assert_rpc_code(
                result.expect_err("reject oversized response"),
                Code::OutOfRange,
            );
        }
        assert_eq!(provider.describe_calls.load(Ordering::SeqCst), 1);
        drop(client);
        server.stop().await;
    }
}

#[tokio::test]
async fn recovery_and_lookup_preserve_provider_errors() {
    let server = TestServer::start(seeded_provider(Fault::LookupUnimplemented).await).await;
    let client = backend(&server);
    assert_eq!(
        recover(&client, "inspection").await,
        State::Unknown(Empty {})
    );

    let mut unserved = identity("inspection");
    unserved.pool.as_mut().expect("recovery pool").pool_id = "unserved-pool".into();
    assert_rpc_code(
        client
            .recover(RecoverRequest {
                operation_identity: Some(unserved),
            })
            .await
            .expect_err("recovery pool is not served"),
        Code::NotFound,
    );

    let allocation = client
        .allocate(allocation_request("lookup-source"))
        .await
        .expect("allocate a handle for lookup")
        .allocation
        .expect("lookup allocation");
    assert_rpc_code(
        client
            .lookup(LookupRequest {
                pool: Some(pool()),
                allocation_id: allocation.allocation_id,
            })
            .await
            .expect_err("lookup is unsupported"),
        Code::Unimplemented,
    );
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn unavailable_and_unsupported_statistics_remain_distinct_errors() {
    for (scenario, provider, supports_stats, expected_code) in [
        (
            "unsupported statistics",
            FaultProvider::new(provider().without_stats(), Fault::None),
            false,
            Code::Unimplemented,
        ),
        (
            "unavailable statistics",
            FaultProvider::new(provider(), Fault::Stats(Code::Unavailable)),
            true,
            Code::Unavailable,
        ),
    ] {
        provider
            .provider
            .define(Request::new(definition()))
            .await
            .expect(scenario);
        let server = TestServer::start(provider.clone()).await;
        let client = backend(&server);
        let description = client.describe(describe_request()).await.expect(scenario);
        assert_eq!(description.supports_stats, supports_stats, "{scenario}");
        assert_rpc_code(
            client.stats(stats_request()).await.expect_err(scenario),
            expected_code,
        );
        assert_eq!(provider.stats_calls.load(Ordering::SeqCst), 1, "{scenario}");
        drop(client);
        server.stop().await;
    }
}

#[tokio::test]
async fn call_deadline_covers_response_headers_and_body() {
    enum Stall {
        Headers,
        Body,
    }

    for stall in [Stall::Headers, Stall::Body] {
        let gate = Arc::new(Gate::default());
        let provider = FaultProvider::new(provider(), Fault::StallDescription(gate.clone()));
        let server = match stall {
            Stall::Headers => TestServer::start(provider.clone()).await,
            Stall::Body => TestServer::start_service(StalledBodyService(gate.clone()), None).await,
        };
        let client = backend(&server).with_timeouts(TEST_TIMEOUT, STALL_TIMEOUT);
        let started = Instant::now();
        let mut description = Box::pin(client.describe(describe_request()));
        tokio::select! {
            result = &mut description => panic!("description finished before its response stalled: {result:?}"),
            () = bounded(gate.entered.notified()) => {}
        }
        let error = bounded(description)
            .await
            .expect_err("description response is stalled");
        assert_deadline_elapsed(started.elapsed());
        match stall {
            Stall::Headers => {
                assert_header_deadline(error);
                let header = provider
                    .grpc_timeout
                    .lock()
                    .expect("timeout header lock")
                    .clone()
                    .expect("grpc-timeout received by provider");
                let timeout = grpc_timeout(&header);
                assert!(!timeout.is_zero());
                assert!(
                    timeout <= STALL_TIMEOUT,
                    "propagated timeout exceeds call budget: {timeout:?}"
                );
            }
            Stall::Body => assert!(matches!(error, GrpcResourcePoolError::DeadlineExceeded)),
        }
        bounded(gate.dropped.notified()).await;
        assert_eq!(gate.calls.load(Ordering::SeqCst), 1);
        drop(client);
        server.stop().await;
    }
}

#[test]
fn rpc_errors_allow_explicit_inspection_without_exposing_remote_contents() {
    let secrets = [
        "provider-message-secret",
        "provider-details-secret",
        "provider-metadata-secret",
    ];
    let mut metadata = tonic::metadata::MetadataMap::new();
    metadata.insert(
        "x-provider-secret",
        secrets[2].parse().expect("ASCII metadata"),
    );
    let status = Status::with_details_and_metadata(
        Code::PermissionDenied,
        secrets[0],
        Bytes::from_static(secrets[1].as_bytes()),
        metadata,
    );
    let rpc = RpcError::from(status);
    assert_eq!(rpc.status().message(), secrets[0]);
    assert_eq!(rpc.status().details(), secrets[1].as_bytes());
    assert_eq!(
        rpc.status()
            .metadata()
            .get("x-provider-secret")
            .expect("provider metadata"),
        secrets[2]
    );
    let unredacted = format!("{:?}", rpc.status());
    for secret in secrets {
        assert!(unredacted.contains(secret), "fixture must contain {secret}");
    }
    let error = GrpcResourcePoolError::Rpc(rpc);
    let mut source: Option<&dyn std::error::Error> = Some(&error);
    while let Some(error) = source {
        for secret in secrets {
            assert!(!error.to_string().contains(secret));
            assert!(!format!("{error:?}").contains(secret));
        }
        source = error.source();
    }
    assert_rpc_code(error, Code::PermissionDenied);
}

#[test]
fn outgoing_rpc_propagates_the_active_trace() {
    thread_local! {
        static WORKER_DISPATCH: std::cell::Cell<Option<tracing::dispatcher::DefaultGuard>> =
            const { std::cell::Cell::new(None) };
    }
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let tracer_provider = SdkTracerProvider::builder().build();
    let subscriber = tracing_subscriber::registry().with(
        tracing_opentelemetry::layer()
            .with_tracer(tracer_provider.tracer("resource-pool-client-test")),
    );
    // Scoped subscribers do not follow spawned threads. Keep this subscriber
    // active on each worker through Tokio's blocking-task destruction, as well
    // as through the current-thread runtime's task and timer cleanup.
    tracing::subscriber::with_default(subscriber, || {
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .on_thread_start(move || {
                WORKER_DISPATCH.set(Some(tracing::dispatcher::set_default(&dispatch)));
            })
            .on_thread_stop(|| drop(WORKER_DISPATCH.take()))
            .enable_all()
            .build()
            .expect("create trace test runtime");
        runtime.block_on(async {
            let provider = FaultProvider::new(provider(), Fault::None);
            let server = TestServer::start(provider.clone()).await;
            let client = backend(&server);
            let span = tracing::info_span!("allocation_request");
            let expected_trace = span.context().span().span_context().trace_id();
            assert_ne!(expected_trace, opentelemetry::trace::TraceId::INVALID);
            client
                .describe(describe_request())
                .instrument(span)
                .await
                .expect("describe inside trace");
            let header = provider
                .traceparent
                .lock()
                .expect("trace header lock")
                .clone()
                .expect("traceparent received over gRPC");
            assert!(
                header.starts_with(&format!("00-{expected_trace}-")),
                "unexpected traceparent: {header}"
            );
            drop(client);
            server.stop().await;
        });
        drop(runtime);
        tracer_provider.shutdown().expect("shut down test tracer");
    });
}

fn backend(server: &TestServer) -> GrpcResourcePoolBackend {
    GrpcResourcePoolBackend::new(server.endpoint.clone(), None)
        .expect("configure provider client")
        .with_timeouts(TEST_TIMEOUT, TEST_TIMEOUT)
}

async fn seeded_provider(fault: Fault) -> FaultProvider {
    let provider = provider();
    provider
        .define(Request::new(definition()))
        .await
        .expect("register test eligibility");
    FaultProvider::new(provider, fault)
}

fn allocation_request(request_id: &str) -> AllocateRequest {
    AllocateRequest {
        operation: Some(operation(request_id)),
        requested_value: None,
    }
}

fn identity(request_id: &str) -> OperationIdentity {
    OperationIdentity {
        pool: Some(pool()),
        request_id: request_id.into(),
    }
}

fn describe_request() -> DescribeRequest {
    let pool = pool();
    DescribeRequest {
        pool_alias: pool.logical_pool,
    }
}

fn stats_request() -> StatsRequest {
    StatsRequest {
        pool: Some(pool()),
        eligibility_revision: "initial".into(),
    }
}

async fn recover(client: &GrpcResourcePoolBackend, request_id: &str) -> State {
    client
        .recover(RecoverRequest {
            operation_identity: Some(identity(request_id)),
        })
        .await
        .expect("recover original operation")
        .operation_status
        .expect("recovery status")
        .state
        .expect("recovery state")
}

async fn stats(client: &GrpcResourcePoolBackend) -> PoolStats {
    client
        .stats(stats_request())
        .await
        .expect("read provider inventory")
        .stats
        .expect("inventory statistics")
}

fn assert_rpc_code(error: GrpcResourcePoolError, expected: Code) {
    let GrpcResourcePoolError::Rpc(error) = error else {
        panic!("expected RPC status {expected:?}, got {error:?}");
    };
    assert_eq!(error.code(), expected);
}

pub(in crate::grpc) fn grpc_timeout(header: &str) -> Duration {
    let (amount, unit) = header.split_at(header.len() - 1);
    let amount: u64 = amount.parse().expect("grpc-timeout amount");
    match unit {
        "H" => Duration::from_secs(amount * 60 * 60),
        "M" => Duration::from_secs(amount * 60),
        "S" => Duration::from_secs(amount),
        "m" => Duration::from_millis(amount),
        "u" => Duration::from_micros(amount),
        "n" => Duration::from_nanos(amount),
        _ => panic!("unknown grpc-timeout unit: {unit}"),
    }
}

fn assert_deadline_elapsed(elapsed: Duration) {
    // grpc-timeout encodes the remaining budget with finite precision.
    assert!(
        elapsed + Duration::from_millis(1) >= STALL_TIMEOUT,
        "call ended early: {elapsed:?}"
    );
    // Allow scheduler jitter while still rejecting a doubled call budget.
    let latest = STALL_TIMEOUT + Duration::from_millis(100);
    assert!(
        elapsed <= latest,
        "call exceeded its budget and scheduling allowance: {elapsed:?}"
    );
}

fn assert_header_deadline(error: GrpcResourcePoolError) {
    // Tonic's propagated timeout and the outer call deadline can race while
    // waiting for headers. Both leave the provider operation unresolved.
    match error {
        GrpcResourcePoolError::DeadlineExceeded => {}
        GrpcResourcePoolError::Rpc(error) => {
            assert!(matches!(
                error.code(),
                Code::Cancelled | Code::DeadlineExceeded
            ));
        }
        error => panic!("expected call timeout, got {error:?}"),
    }
}

// Reuse the provider contract's semantic assertions through the production
// client. Only this test facade converts client failures back to tonic Status.
struct RemoteProvider(GrpcResourcePoolBackend, Arc<AtomicUsize>);

fn status(error: GrpcResourcePoolError) -> Status {
    match error {
        GrpcResourcePoolError::Rpc(error) => error.into_status(),
        GrpcResourcePoolError::DeadlineExceeded => Status::deadline_exceeded("local call deadline"),
        error => Status::internal(error.to_string()),
    }
}

#[tonic::async_trait]
impl ResourcePool for RemoteProvider {
    async fn describe(
        &self,
        request: Request<DescribeRequest>,
    ) -> Result<Response<DescribeResponse>, Status> {
        self.0
            .describe(request.into_inner())
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn define(
        &self,
        request: Request<DefineRequest>,
    ) -> Result<Response<DefineResponse>, Status> {
        self.0
            .define(request.into_inner())
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn allocate(
        &self,
        request: Request<AllocateRequest>,
    ) -> Result<Response<AllocateResponse>, Status> {
        let calls = self.1.load(Ordering::SeqCst);
        let result = self.0.allocate(request.into_inner()).await;
        assert_eq!(
            self.1.load(Ordering::SeqCst),
            calls + 1,
            "one provider call per Allocate request"
        );
        result.map(Response::new).map_err(status)
    }

    async fn allocate_exact(
        &self,
        request: Request<AllocateExactRequest>,
    ) -> Result<Response<AllocateExactResponse>, Status> {
        self.0
            .allocate_exact(request.into_inner())
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn recover(
        &self,
        request: Request<RecoverRequest>,
    ) -> Result<Response<RecoverResponse>, Status> {
        self.0
            .recover(request.into_inner())
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn cancel(
        &self,
        request: Request<CancelRequest>,
    ) -> Result<Response<CancelResponse>, Status> {
        self.0
            .cancel(request.into_inner())
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn lookup(
        &self,
        request: Request<LookupRequest>,
    ) -> Result<Response<LookupResponse>, Status> {
        self.0
            .lookup(request.into_inner())
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn release(
        &self,
        request: Request<ReleaseRequest>,
    ) -> Result<Response<ReleaseResponse>, Status> {
        self.0
            .release(request.into_inner())
            .await
            .map(Response::new)
            .map_err(status)
    }

    async fn stats(
        &self,
        request: Request<StatsRequest>,
    ) -> Result<Response<StatsResponse>, Status> {
        self.0
            .stats(request.into_inner())
            .await
            .map(Response::new)
            .map_err(status)
    }
}
