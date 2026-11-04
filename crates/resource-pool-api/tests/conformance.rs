// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use carbide_resource_pool_api::testing::{LocalProvider, verify_provider};
use carbide_resource_pool_api::v1::eligibility::Entries;
use carbide_resource_pool_api::v1::operation_status::State;
use carbide_resource_pool_api::v1::resource_pool_client::ResourcePoolClient;
use carbide_resource_pool_api::v1::resource_pool_server::{ResourcePool, ResourcePoolServer};
use carbide_resource_pool_api::v1::value::Kind;
use carbide_resource_pool_api::v1::{
    AllocateExactRequest, AllocateRequest, Allocation, AllocationOperation, AssignmentMode,
    CancelRequest, DefineRequest, DescribeRequest, Eligibility, Empty, Failure, FailureCode,
    Ipv6Prefix, LookupRequest, OperationIdentity, Owner, PoolBinding, RecoverRequest,
    ReleaseRequest, RouteTarget, RouteTargetMode, RouteTargetPolicy, RouteTargets, StatsRequest,
    Value, ValueDomain,
};
use carbide_test_support::{Case, Outcome, check_cases};
use prost::Message;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::{Code, Request};

#[test]
fn protobuf_preserves_omitted_and_empty_route_target_lists() {
    let policies = [
        ("omitted policy", None),
        (
            "omitted imports and explicit empty exports",
            Some(RouteTargetPolicy {
                mode: RouteTargetMode::PreserveGenerated as i32,
                imports: None,
                exports: Some(RouteTargets { values: Vec::new() }),
                peer_identity: Vec::new(),
            }),
        ),
        (
            "explicit empty replacement",
            Some(RouteTargetPolicy {
                mode: RouteTargetMode::ReplaceGenerated as i32,
                imports: Some(RouteTargets { values: Vec::new() }),
                exports: Some(RouteTargets { values: Vec::new() }),
                peer_identity: Vec::new(),
            }),
        ),
    ];
    check_cases(
        policies.into_iter().map(|(scenario, route_target_policy)| {
            let allocation = Allocation {
                route_target_policy,
                ..Default::default()
            };
            Case {
                scenario,
                input: allocation.clone(),
                expect: Outcome::Yields(allocation),
            }
        }),
        |allocation| {
            Allocation::decode(allocation.encode_to_vec().as_slice())
                .map_err(|error| error.to_string())
        },
    );
}

#[tokio::test]
async fn local_provider_satisfies_shared_contract() {
    let pool = pool();
    let policy = route_target_policy();
    let provider = LocalProvider::new(
        pool.clone(),
        ValueDomain::Integer,
        (1..=5).map(Into::into).collect(),
        Some(policy.clone()),
    )
    .expect("create conformance inventory");
    verify_provider(&provider, pool, Some(policy)).await;
}

#[tokio::test]
async fn describe_preserves_unicode_and_rejects_nul_aliases() {
    let pool = PoolBinding {
        authority_id: " authority Δ ".into(),
        pool_id: " pool 池 ".into(),
        logical_pool: " vpc-vni Δ ".into(),
        ..pool()
    };
    let provider = LocalProvider::new(pool.clone(), ValueDomain::Integer, vec![], None)
        .expect("create provider with opaque identifiers");
    for (scenario, alias, expected) in [
        ("exact Unicode and whitespace", " vpc-vni Δ ", None),
        (
            "no whitespace normalization",
            "vpc-vni Δ",
            Some(Code::NotFound),
        ),
        (
            "NUL is invalid",
            " vpc-vni Δ\0 ",
            Some(Code::InvalidArgument),
        ),
    ] {
        let result = provider
            .describe(Request::new(DescribeRequest {
                pool_alias: alias.into(),
            }))
            .await;
        match expected {
            Some(code) => assert_eq!(result.expect_err(scenario).code(), code, "{scenario}"),
            None => {
                let response = result.expect(scenario).into_inner();
                assert_eq!(response.authority_id, pool.authority_id, "{scenario}");
                assert_eq!(response.pool_id, pool.pool_id, "{scenario}");
            }
        }
    }
}

#[tokio::test]
async fn unsupported_statistics_are_not_empty_inventory() {
    let pool = pool();
    let provider = LocalProvider::new(pool.clone(), ValueDomain::Integer, vec![1.into()], None)
        .expect("create provider without statistics")
        .without_stats();
    provider
        .define(Request::new(definition(
            &pool,
            ValueDomain::Integer,
            vec![1.into()],
        )))
        .await
        .expect("register eligibility before requesting unsupported statistics");
    let description = provider
        .describe(Request::new(DescribeRequest {
            pool_alias: pool.logical_pool.clone(),
        }))
        .await
        .expect("describe provider capabilities")
        .into_inner();
    assert!(!description.supports_stats);
    assert_eq!(
        provider
            .stats(Request::new(StatsRequest {
                pool: Some(pool),
                eligibility_revision: "initial".into()
            }))
            .await
            .expect_err("unsupported statistics are an error")
            .code(),
        Code::Unimplemented
    );
}

#[tokio::test]
async fn generated_client_and_server_preserve_allocation_identity_and_policy() {
    let pool = pool();
    let policy = route_target_policy();
    let provider = LocalProvider::new(
        pool.clone(),
        ValueDomain::Integer,
        vec![1.into()],
        Some(policy.clone()),
    )
    .expect("create loopback provider");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind provider listener");
    let address = listener.local_addr().expect("read provider address");
    let shutdown = CancellationToken::new();
    let server = tonic::transport::Server::builder().add_service(ResourcePoolServer::new(provider));

    // Bound the loopback test so a broken exchange or shutdown cannot hang CI.
    // Production client deadlines and credentials belong to the client adapter.
    let (client_result, server_result) = tokio::join!(
        async {
            let result = tokio::time::timeout(Duration::from_secs(10), async {
                let mut client = ResourcePoolClient::connect(format!("http://{address}"))
                    .await
                    .expect("connect generated client");
                let description = client
                    .describe(DescribeRequest {
                        pool_alias: pool.logical_pool.clone(),
                    })
                    .await
                    .expect("describe over gRPC")
                    .into_inner();
                assert_eq!(description.authority_id, pool.authority_id);
                client
                    .define(definition(&pool, ValueDomain::Integer, vec![1.into()]))
                    .await
                    .expect("define over gRPC");
                let malformed = AllocateRequest {
                    operation: Some(operation(&pool, "malformed-wire-allocation")),
                    requested_value: Some(Value::default()),
                };
                let error = client
                    .allocate(malformed.clone())
                    .await
                    .expect_err("reject a present value with no kind over gRPC");
                assert_eq!(error.code(), Code::InvalidArgument);
                let replay = client
                    .allocate(malformed)
                    .await
                    .expect_err("replay the malformed request's saved failure");
                assert_eq!(
                    (replay.code(), replay.message()),
                    (error.code(), error.message())
                );
                let failed_identity = OperationIdentity {
                    pool: Some(pool.clone()),
                    request_id: "malformed-wire-allocation".into(),
                };
                let recovered = client
                    .recover(RecoverRequest {
                        operation_identity: Some(failed_identity.clone()),
                    })
                    .await
                    .expect("recover the rejected allocation over gRPC")
                    .into_inner();
                assert_eq!(recovered.operation_identity, Some(failed_identity));
                assert_eq!(
                    recovered
                        .operation_status
                        .expect("recovery includes its status")
                        .state,
                    Some(State::Failed(Failure {
                        code: FailureCode::InvalidArgument as i32,
                        message: error.message().into(),
                    }))
                );

                // The malformed request must leave the only inventory value free.
                let operation = operation(&pool, "wire-allocation");
                let allocation = client
                    .allocate(AllocateRequest {
                        operation: Some(operation.clone()),
                        requested_value: None,
                    })
                    .await
                    .expect("allocate over gRPC")
                    .into_inner()
                    .allocation
                    .expect("allocation response includes its assignment");
                assert_eq!(allocation.operation, Some(operation.clone()));
                assert_eq!(allocation.value, Some(1.into()));
                assert_eq!(allocation.route_target_policy, Some(policy));

                for (scenario, result) in [
                    (
                        "recover without an operation identity",
                        client
                            .recover(RecoverRequest {
                                operation_identity: None,
                            })
                            .await
                            .map(|_| ()),
                    ),
                    (
                        "cancel without an operation identity",
                        client
                            .cancel(CancelRequest {
                                operation_identity: None,
                            })
                            .await
                            .map(|_| ()),
                    ),
                ] {
                    assert_eq!(result.expect_err(scenario).code(), Code::InvalidArgument);
                }
                let identity = OperationIdentity {
                    pool: Some(pool.clone()),
                    request_id: operation.request_id,
                };
                let recovered = client
                    .recover(RecoverRequest {
                        operation_identity: Some(identity.clone()),
                    })
                    .await
                    .expect("invalid recovery and cancellation leave the allocation unchanged")
                    .into_inner();
                assert_eq!(recovered.operation_identity, Some(identity.clone()));
                assert_eq!(
                    recovered
                        .operation_status
                        .expect("recovery includes its status")
                        .state,
                    Some(State::Allocated(allocation.clone()))
                );
                assert!(
                    client
                        .release(ReleaseRequest {
                            pool: Some(pool),
                            allocation_id: allocation.allocation_id,
                            owner: operation.owner
                        })
                        .await
                        .expect("release over gRPC")
                        .into_inner()
                        .applied
                );
                let cancelled = client
                    .cancel(CancelRequest {
                        operation_identity: Some(identity.clone()),
                    })
                    .await
                    .expect("cancel a released operation over gRPC")
                    .into_inner();
                assert_eq!(cancelled.operation_identity, Some(identity));
                assert_eq!(
                    cancelled
                        .operation_status
                        .expect("cancellation includes its status")
                        .state,
                    Some(State::Released(Empty {}))
                );
            })
            .await;
            shutdown.cancel();
            result
        },
        tokio::time::timeout(
            Duration::from_secs(15),
            server.serve_with_incoming_shutdown(
                TcpListenerStream::new(listener),
                shutdown.cancelled(),
            )
        ),
    );
    client_result.expect("provider exchange completed within 10 seconds");
    server_result
        .expect("provider stopped within 15 seconds")
        .expect("serve provider RPCs");
}

/// Bindings share inventory and its usage counts, while lookup stays limited
/// to the binding that allocated the value.
#[tokio::test]
async fn bindings_share_inventory_with_distinct_request_ids() {
    let first_pool = pool();
    let second_pool = PoolBinding {
        binding_id: "second-binding".into(),
        ..first_pool.clone()
    };
    let provider = LocalProvider::new(
        first_pool.clone(),
        ValueDomain::Integer,
        vec![1.into(), 2.into(), 3.into()],
        None,
    )
    .expect("create inventory shared by two bindings");
    for pool in [&first_pool, &second_pool] {
        provider
            .define(Request::new(definition(
                pool,
                ValueDomain::Integer,
                vec![1.into(), 2.into(), 3.into()],
            )))
            .await
            .expect("register binding eligibility");
    }
    let first = provider
        .allocate(Request::new(AllocateRequest {
            operation: Some(operation(&first_pool, "first-request")),
            requested_value: None,
        }))
        .await
        .expect("allocate through the first binding")
        .into_inner()
        .allocation
        .expect("allocation response includes its assignment");
    let second = provider
        .allocate(Request::new(AllocateRequest {
            operation: Some(operation(&second_pool, "second-request")),
            requested_value: None,
        }))
        .await
        .expect("allocate through the second binding with its own request ID")
        .into_inner()
        .allocation
        .expect("allocation response includes its assignment");
    assert_ne!(first.value, second.value);
    assert_ne!(first.allocation_id, second.allocation_id);
    assert_eq!(
        provider
            .lookup(Request::new(LookupRequest {
                pool: Some(second_pool.clone()),
                allocation_id: first.allocation_id.clone(),
            }))
            .await
            .expect_err("one binding cannot look up another binding's allocation")
            .code(),
        Code::NotFound
    );
    for pool in [&first_pool, &second_pool] {
        let stats = provider
            .stats(Request::new(StatsRequest {
                pool: Some(pool.clone()),
                eligibility_revision: "initial".into(),
            }))
            .await
            .expect("count allocations from both bindings")
            .into_inner()
            .stats
            .expect("statistics response includes its counts");
        assert_eq!((stats.used, stats.free), (2, 1));
    }
}

#[tokio::test]
async fn provider_operations_preserve_address_and_prefix_values() {
    let cases = [
        (
            "mixed IPv4 and IPv6 addresses",
            ValueDomain::IpAddress,
            vec![
                Ipv4Addr::new(192, 0, 2, 1).into(),
                Ipv6Addr::LOCALHOST.into(),
            ],
        ),
        (
            "IPv6 delegated prefix",
            ValueDomain::Ipv6Prefix,
            vec![Value {
                kind: Some(Kind::Ipv6Prefix(Ipv6Prefix {
                    network: Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0)
                        .octets()
                        .to_vec(),
                    prefix_len: 64,
                })),
            }],
        ),
    ];
    for (scenario, domain, values) in cases {
        let pool = pool();
        let provider =
            LocalProvider::new(pool.clone(), domain, values.clone(), None).expect(scenario);
        provider
            .define(Request::new(definition(&pool, domain, values.clone())))
            .await
            .expect(scenario);
        for (index, value) in values.iter().enumerate() {
            let allocation = provider
                .allocate_exact(Request::new(AllocateExactRequest {
                    operation: Some(operation(&pool, &format!("domain-allocation-{index}"))),
                    value: Some(value.clone()),
                }))
                .await
                .expect(scenario)
                .into_inner()
                .allocation
                .expect("exact allocation response includes its assignment");
            assert_eq!(allocation.value.as_ref(), Some(value), "{scenario}");
            assert_eq!(
                allocation.assignment_mode,
                AssignmentMode::Automatic as i32,
                "{scenario}"
            );
            assert_eq!(allocation.route_target_policy, None, "{scenario}");
        }
        let stats = provider
            .stats(Request::new(StatsRequest {
                pool: Some(pool),
                eligibility_revision: "initial".into(),
            }))
            .await
            .expect(scenario)
            .into_inner()
            .stats
            .expect("statistics response includes its counts");
        assert_eq!(
            (stats.used, stats.free),
            (values.len() as u64, 0),
            "{scenario}"
        );
    }
}

#[tokio::test]
async fn empty_inventory_still_rejects_malformed_definitions() {
    let pool = pool();
    let provider = LocalProvider::new(pool.clone(), ValueDomain::Integer, Vec::new(), None)
        .expect("create an empty provider inventory");
    let valid = definition(&pool, ValueDomain::Integer, vec![1.into()]);
    let mut malformed = valid.clone();
    malformed.eligibility[0].entries = None;
    assert_eq!(
        provider
            .define(Request::new(malformed))
            .await
            .expect_err("definitions are validated even without inventory to check")
            .code(),
        Code::InvalidArgument
    );
    provider
        .define(Request::new(valid))
        .await
        .expect("the rejected definition must not reserve its revision");
    let stats = provider
        .stats(Request::new(StatsRequest {
            pool: Some(pool),
            eligibility_revision: "initial".into(),
        }))
        .await
        .expect("valid eligibility does not create provider inventory")
        .into_inner()
        .stats
        .expect("statistics response includes its counts");
    assert_eq!((stats.used, stats.free), (0, 0));
}

fn definition(pool: &PoolBinding, value_domain: ValueDomain, values: Vec<Value>) -> DefineRequest {
    DefineRequest {
        pool: Some(pool.clone()),
        eligibility_revision: "initial".into(),
        previous_revision: None,
        value_domain: value_domain as i32,
        eligibility: values
            .into_iter()
            .map(|value| Eligibility {
                assignment_mode: AssignmentMode::Automatic as i32,
                entries: Some(Entries::Value(value)),
            })
            .collect(),
    }
}

fn operation(pool: &PoolBinding, request_id: &str) -> AllocationOperation {
    AllocationOperation {
        pool: Some(pool.clone()),
        request_id: request_id.into(),
        eligibility_revision: "initial".into(),
        owner: Some(Owner {
            owner_type: "vpc".into(),
            owner_id: "test-vpc".into(),
            slot: "value".into(),
            generation: 0,
        }),
    }
}

fn pool() -> PoolBinding {
    PoolBinding {
        authority_id: "conformance-provider".into(),
        pool_id: "provider-vnis".into(),
        binding_id: "vpc-binding".into(),
        logical_pool: "vpc-vni".into(),
    }
}

fn route_target_policy() -> RouteTargetPolicy {
    let target = RouteTarget { asn: 64512, vni: 1 };
    RouteTargetPolicy {
        mode: RouteTargetMode::ReplaceGenerated as i32,
        imports: Some(RouteTargets {
            values: vec![target],
        }),
        exports: Some(RouteTargets {
            values: vec![target],
        }),
        peer_identity: vec![target],
    }
}
