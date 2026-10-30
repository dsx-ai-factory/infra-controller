// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use tonic::{Code, Request, Status};

use crate::v1::eligibility::Entries;
use crate::v1::operation_status::State;
use crate::v1::resource_pool_server::ResourcePool;
use crate::v1::{
    AllocateExactRequest, AllocateRequest, Allocation, AllocationOperation, AssignmentMode,
    CancelRequest, DefineRequest, DescribeRequest, Eligibility, Empty, Failure, FailureCode,
    LookupRequest, OperationIdentity, OperationStatus, Owner, PoolBinding, PoolStats,
    RecoverRequest, ReleaseRequest, RouteTargetPolicy, StatsRequest, ValueDomain, ValueRange,
};

/// `verify_provider` checks allocation, replay, cleanup, and growth through the
/// generated in-process service trait. A failed check panics with the violated
/// contract. A remote client needs an adapter to this trait.
///
/// Give it a fresh integer pool containing exactly the available values `1..=5`,
/// with no definitions or request history. `Describe` must resolve `logical_pool`.
/// Every allocation must return `expected_route_target_policy` unchanged. The
/// checks define automatic values 1 and 2, manual values 3 and 4, then grow the
/// definition to include automatic values 5 and 6. Value 6 is eligible but absent
/// from provider inventory. No allocations remain afterward. Definitions and
/// terminal request records remain for replay checks.
///
/// Run this on a Tokio runtime with its time driver enabled. Cancellation may
/// return `pending`; each cancellation check has a five-second budget covering
/// the initial `Cancel` call and all `Recover` polls, spaced 10 ms apart.
///
/// The allocation/cancellation checks exercise both sequential arrival orders.
/// `LocalProvider` completes each call immediately. Asynchronous provisioning,
/// persistence across restarts, and recovery of NICo's database transactions
/// require separate integration tests.
pub async fn verify_provider(
    provider: &impl ResourcePool,
    pool: PoolBinding,
    expected_route_target_policy: Option<RouteTargetPolicy>,
) {
    let description = provider
        .describe(Request::new(DescribeRequest {
            pool_alias: pool.logical_pool.clone(),
        }))
        .await
        .expect("describe conformance pool")
        .into_inner();
    assert_eq!(description.authority_id, pool.authority_id);
    assert_eq!(description.pool_id, pool.pool_id);
    assert_eq!(description.value_domain, ValueDomain::Integer as i32);

    let definition = DefineRequest {
        pool: Some(pool.clone()),
        eligibility_revision: "initial".into(),
        previous_revision: None,
        value_domain: ValueDomain::Integer as i32,
        eligibility: vec![
            Eligibility {
                assignment_mode: AssignmentMode::Automatic as i32,
                entries: Some(Entries::Range(ValueRange {
                    start: Some(1.into()),
                    end: Some(3.into()),
                })),
            },
            Eligibility {
                assignment_mode: AssignmentMode::Manual as i32,
                entries: Some(Entries::Range(ValueRange {
                    start: Some(3.into()),
                    end: Some(5.into()),
                })),
            },
        ],
    };
    provider
        .define(Request::new(definition.clone()))
        .await
        .expect("define initial eligibility");
    replay_and_release(
        provider,
        &pool,
        &expected_route_target_policy,
        description.supports_stats,
    )
    .await;
    cancellation_prevents_late_allocation(
        provider,
        &pool,
        &expected_route_target_policy,
        description.supports_stats,
    )
    .await;
    growth_preserves_prior_definitions(
        provider,
        definition,
        &expected_route_target_policy,
        description.supports_stats,
    )
    .await;
}

async fn replay_and_release(
    provider: &impl ResourcePool,
    pool: &PoolBinding,
    expected_policy: &Option<RouteTargetPolicy>,
    supports_stats: bool,
) {
    let mut invalid_owner = operation(pool, "nul-owner");
    invalid_owner.owner.as_mut().expect("owner").owner_id = "vpc\0id".into();
    assert_eq!(
        provider
            .allocate(Request::new(AllocateRequest {
                operation: Some(invalid_owner),
                requested_value: None,
            }))
            .await
            .expect_err("a fresh allocation rejects NUL in its owner")
            .code(),
        Code::InvalidArgument
    );

    let wrong_partition_operation = operation(pool, "wrong-partition");
    let wrong_partition = provider
        .allocate(Request::new(AllocateRequest {
            operation: Some(wrong_partition_operation.clone()),
            requested_value: Some(2.into()),
        }))
        .await
        .expect_err("automatic value is not available for requested create");
    assert_eq!(wrong_partition.code(), Code::FailedPrecondition);
    assert_eq!(
        recover(provider, operation_identity(&wrong_partition_operation))
            .await
            .expect("recover the original selection failure")
            .state,
        Some(State::Failed(Failure {
            code: FailureCode::FailedPrecondition as i32,
            message: wrong_partition.message().into(),
        }))
    );
    let request = AllocateRequest {
        operation: Some(operation(pool, "replay")),
        requested_value: None,
    };
    // Treat the response as lost. Recovery must find the original assignment,
    // including the handle and RT policy needed to finish creating the VPC.
    let first_reply = provider
        .allocate(Request::new(request.clone()))
        .await
        .expect("allocate before lost reply")
        .into_inner()
        .allocation
        .expect("allocation response includes its allocation");
    let recovered = recover(
        provider,
        operation_identity(request.operation.as_ref().expect("operation")),
    )
    .await
    .expect("recover lost reply");
    let Some(State::Allocated(first)) = recovered.state else {
        panic!("recovery must return the original allocation");
    };
    assert_eq!(first, first_reply);
    assert_eq!(first.operation, request.operation);
    assert!(!first.allocation_id.is_empty());
    assert!([1.into(), 2.into()].contains(first.value.as_ref().expect("allocated value")));
    assert_eq!(&first.route_target_policy, expected_policy);
    assert_eq!(first.assignment_mode, AssignmentMode::Automatic as i32);
    assert_eq!(
        provider
            .allocate(Request::new(request.clone()))
            .await
            .expect("replay allocation")
            .into_inner()
            .allocation
            .expect("allocation response includes its allocation"),
        first
    );

    let original = request.operation.as_ref().expect("operation");
    let mut changed_owner = original.clone();
    changed_owner.owner.as_mut().expect("owner").owner_id = "another-vpc".into();
    let mut changed_owner_type = original.clone();
    changed_owner_type.owner.as_mut().expect("owner").owner_type = "machine".into();
    let mut invalid_owner_type = original.clone();
    invalid_owner_type.owner.as_mut().expect("owner").owner_type = "machine\0type".into();
    let mut changed_slot = original.clone();
    changed_slot.owner.as_mut().expect("owner").slot = "another-slot".into();
    let mut changed_generation = original.clone();
    changed_generation.owner.as_mut().expect("owner").generation += 1;
    let mut changed_binding = original.clone();
    changed_binding.pool.as_mut().expect("pool").binding_id = "another-binding".into();
    let mut changed_pool = original.clone();
    changed_pool.pool.as_mut().expect("pool").logical_pool = "external-vpc-vni".into();
    let mut changed_provider_pool = original.clone();
    changed_provider_pool.pool.as_mut().expect("pool").pool_id = "another-provider-pool".into();
    let mut changed_revision = original.clone();
    changed_revision.eligibility_revision = "another-revision".into();
    for (scenario, changed) in [
        ("owner", changed_owner),
        ("owner type", changed_owner_type),
        (
            "invalid owner type still conflicts on replay",
            invalid_owner_type,
        ),
        ("slot", changed_slot),
        ("generation", changed_generation),
        ("binding", changed_binding),
        ("logical pool", changed_pool),
        ("provider pool", changed_provider_pool),
        ("revision", changed_revision),
    ] {
        assert_eq!(
            provider
                .allocate(Request::new(AllocateRequest {
                    operation: Some(changed),
                    requested_value: None
                }))
                .await
                .expect_err(scenario)
                .code(),
            Code::AlreadyExists,
            "{scenario}"
        );
    }
    assert_eq!(
        provider
            .allocate(Request::new(AllocateRequest {
                requested_value: Some(3.into()),
                ..request.clone()
            }))
            .await
            .expect_err("changed selection")
            .code(),
        Code::AlreadyExists
    );
    assert_eq!(
        provider
            .allocate_exact(Request::new(AllocateExactRequest {
                operation: request.operation.clone(),
                value: first.value.clone()
            }))
            .await
            .expect_err("changed allocation method")
            .code(),
        Code::AlreadyExists
    );

    // Recovery and cancellation must check the binding too; a reused request ID
    // cannot inspect or cancel another binding's allocation.
    let original_identity = operation_identity(original);
    let mut wrong_binding = original_identity.clone();
    wrong_binding.pool.as_mut().expect("pool").binding_id = "another-binding".into();
    for (scenario, result) in [
        (
            "recover with a changed binding",
            provider
                .recover(Request::new(RecoverRequest {
                    operation_identity: Some(wrong_binding.clone()),
                }))
                .await
                .map(|_| ()),
        ),
        (
            "cancel with a changed binding",
            provider
                .cancel(Request::new(CancelRequest {
                    operation_identity: Some(wrong_binding),
                }))
                .await
                .map(|_| ()),
        ),
    ] {
        assert_eq!(result.expect_err(scenario).code(), Code::AlreadyExists);
    }
    assert_eq!(
        recover(provider, original_identity)
            .await
            .expect("a rejected cancellation preserves the original allocation")
            .state,
        Some(State::Allocated(first.clone()))
    );

    let mut second_operation = operation(pool, "second-slot");
    second_operation.owner.as_mut().expect("owner").slot = "another-slot".into();
    let second = provider
        .allocate(Request::new(AllocateRequest {
            operation: Some(second_operation.clone()),
            requested_value: None,
        }))
        .await
        .expect("allocate another slot for the same owner")
        .into_inner()
        .allocation
        .expect("allocation response includes its allocation");
    assert_ne!(second.value, first.value);
    assert_eq!(second.operation, Some(second_operation));
    assert_eq!(&second.route_target_policy, expected_policy);
    assert_eq!(
        provider
            .allocate_exact(Request::new(AllocateExactRequest {
                operation: Some(operation(pool, "occupied")),
                value: first.value.clone()
            }))
            .await
            .expect_err("same owner cannot take an occupied value")
            .code(),
        Code::FailedPrecondition
    );
    check_stats(provider, pool, "initial", supports_stats, [2, 0, 0, 2]).await;
    let exhausted_operation = operation(pool, "automatic-exhausted");
    let failure = provider
        .allocate(Request::new(AllocateRequest {
            operation: Some(exhausted_operation.clone()),
            requested_value: None,
        }))
        .await
        .expect_err("automatic partition is exhausted");
    assert_eq!(failure.code(), Code::ResourceExhausted);

    // Keep both automatic values allocated while we fill the manual partition.
    // Exact allocation must retain its error contract even when the pool is full.
    requested_values_preserve_partitions(provider, pool, expected_policy, supports_stats).await;

    let release = release_request(&first);
    let mut wrong_owner = release.clone();
    wrong_owner.owner.as_mut().expect("owner").owner_id = "another-vpc".into();
    let mut wrong_owner_type = release.clone();
    wrong_owner_type.owner.as_mut().expect("owner").owner_type = "machine".into();
    let mut wrong_slot = release.clone();
    wrong_slot.owner.as_mut().expect("owner").slot = "another-slot".into();
    let mut wrong_generation = release.clone();
    wrong_generation.owner.as_mut().expect("owner").generation += 1;
    let mut wrong_binding = release.clone();
    wrong_binding.pool.as_mut().expect("pool").binding_id = "another-binding".into();
    for (scenario, mismatched) in [
        ("owner", wrong_owner),
        ("owner type", wrong_owner_type),
        ("slot", wrong_slot),
        ("generation", wrong_generation),
        ("binding", wrong_binding),
    ] {
        assert!(
            !provider
                .release(Request::new(mismatched))
                .await
                .expect(scenario)
                .into_inner()
                .applied,
            "{scenario}"
        );
    }
    assert_eq!(
        lookup(provider, &first)
            .await
            .expect("lookup active allocation"),
        first
    );
    assert!(
        provider
            .release(Request::new(release.clone()))
            .await
            .expect("release original allocation")
            .into_inner()
            .applied
    );
    assert_eq!(
        lookup(provider, &first)
            .await
            .expect_err("released handle is no longer active")
            .code(),
        Code::NotFound
    );
    let replacement_request = AllocateExactRequest {
        operation: Some(operation(pool, "replacement")),
        value: first.value.clone(),
    };
    let replacement = provider
        .allocate_exact(Request::new(replacement_request.clone()))
        .await
        .expect("reallocate released value to the same owner")
        .into_inner()
        .allocation
        .expect("allocation response includes its allocation");
    assert_ne!(replacement.allocation_id, first.allocation_id);
    assert_eq!(replacement.value, first.value);
    assert_eq!(replacement.assignment_mode, first.assignment_mode);
    assert_eq!(&replacement.route_target_policy, expected_policy);
    assert_eq!(
        provider
            .allocate_exact(Request::new(replacement_request.clone()))
            .await
            .expect("replay exact allocation")
            .into_inner()
            .allocation
            .expect("allocation response includes its allocation"),
        replacement
    );
    assert_eq!(
        provider
            .allocate_exact(Request::new(AllocateExactRequest {
                value: Some(3.into()),
                ..replacement_request.clone()
            }))
            .await
            .expect_err("exact request cannot change its value")
            .code(),
        Code::AlreadyExists
    );
    assert!(
        !provider
            .release(Request::new(release))
            .await
            .expect("ignore delayed old release")
            .into_inner()
            .applied
    );
    assert_eq!(
        provider
            .allocate(Request::new(AllocateRequest {
                requested_value: Some(3.into()),
                ..request.clone()
            }))
            .await
            .expect_err("released request still rejects changed inputs")
            .code(),
        Code::AlreadyExists
    );
    assert_eq!(
        lookup(provider, &replacement)
            .await
            .expect("lookup active allocation"),
        replacement
    );
    assert_eq!(
        provider
            .allocate(Request::new(request))
            .await
            .expect_err("released request cannot allocate again")
            .code(),
        Code::FailedPrecondition
    );
    assert_eq!(
        recover(
            provider,
            operation_identity(first.operation.as_ref().expect("operation"))
        )
        .await
        .expect("recover released request")
        .state,
        Some(State::Released(Empty {}))
    );
    let released_identity = operation_identity(first.operation.as_ref().expect("operation"));
    let cancelled = provider
        .cancel(Request::new(CancelRequest {
            operation_identity: Some(released_identity.clone()),
        }))
        .await
        .expect("cancelling a released request preserves its state")
        .into_inner();
    assert_eq!(
        cancelled.operation_identity,
        Some(released_identity),
        "cancel response identifies the released operation"
    );
    assert_eq!(
        cancelled
            .operation_status
            .expect("cancel response includes its operation status")
            .state,
        Some(State::Released(Empty {}))
    );
    for allocation in [second, replacement] {
        assert!(
            provider
                .release(Request::new(release_request(&allocation)))
                .await
                .expect("release active allocation")
                .into_inner()
                .applied
        );
    }
    assert_eq!(
        provider
            .allocate_exact(Request::new(replacement_request))
            .await
            .expect_err("released exact request cannot allocate again")
            .code(),
        Code::FailedPrecondition
    );
}

async fn requested_values_preserve_partitions(
    provider: &impl ResourcePool,
    pool: &PoolBinding,
    expected_policy: &Option<RouteTargetPolicy>,
    supports_stats: bool,
) {
    let mut manual = Vec::new();
    for (request_id, value) in [("manual-three", 3), ("manual-four", 4)] {
        let allocation = provider
            .allocate(Request::new(AllocateRequest {
                operation: Some(operation(pool, request_id)),
                requested_value: Some(value.into()),
            }))
            .await
            .expect("allocate manual value")
            .into_inner()
            .allocation
            .expect("allocation response includes its allocation");
        assert_eq!(allocation.value, Some(value.into()));
        assert_eq!(allocation.assignment_mode, AssignmentMode::Manual as i32);
        assert_eq!(&allocation.route_target_policy, expected_policy);
        manual.push(allocation);
    }
    let exhausted_request = AllocateRequest {
        operation: Some(operation(pool, "manual-exhausted")),
        requested_value: Some(2.into()),
    };
    let failure = provider
        .allocate(Request::new(exhausted_request.clone()))
        .await
        .expect_err("manual exhaustion takes precedence over the requested value");
    assert_eq!(failure.code(), Code::ResourceExhausted);
    assert_eq!(
        recover(
            provider,
            operation_identity(exhausted_request.operation.as_ref().expect("operation"))
        )
        .await
        .expect("recover allocation failure")
        .state,
        Some(State::Failed(Failure {
            code: FailureCode::ResourceExhausted as i32,
            message: failure.message().into(),
        }))
    );
    assert_eq!(
        provider
            .allocate_exact(Request::new(AllocateExactRequest {
                operation: Some(operation(pool, "exact-occupied-full")),
                value: Some(3.into()),
            }))
            .await
            .expect_err("exact allocation rejects an occupied value even when the pool is full")
            .code(),
        Code::FailedPrecondition
    );
    check_stats(provider, pool, "initial", supports_stats, [2, 0, 2, 0]).await;
    for allocation in manual {
        assert!(
            provider
                .release(Request::new(release_request(&allocation)))
                .await
                .expect("release manual value")
                .into_inner()
                .applied
        );
    }
    let replay = provider
        .allocate(Request::new(exhausted_request.clone()))
        .await
        .expect_err("free capacity does not change the saved failure");
    assert_eq!(replay.code(), failure.code());
    assert_eq!(replay.message(), failure.message());
    let failed_identity =
        operation_identity(exhausted_request.operation.as_ref().expect("operation"));
    cancel(provider, failed_identity.clone()).await;
    assert_eq!(
        recover(provider, failed_identity)
            .await
            .expect("recover cancelled failure")
            .state,
        Some(State::Cancelled(Empty {}))
    );
    assert_eq!(
        provider
            .allocate(Request::new(exhausted_request))
            .await
            .expect_err("cancelled failed request cannot allocate again")
            .code(),
        Code::FailedPrecondition
    );
}

async fn cancellation_prevents_late_allocation(
    provider: &impl ResourcePool,
    pool: &PoolBinding,
    expected_policy: &Option<RouteTargetPolicy>,
    supports_stats: bool,
) {
    let late_operation = operation(pool, "cancel-before-allocate");
    let late_identity = operation_identity(&late_operation);
    assert_eq!(
        recover(provider, late_identity.clone())
            .await
            .expect("recover unknown operation")
            .state,
        Some(State::Unknown(Empty {}))
    );
    cancel(provider, late_identity.clone()).await;
    let mut changed_binding = late_operation.clone();
    changed_binding.pool.as_mut().expect("pool").binding_id = "another-binding".into();
    assert_eq!(
        provider
            .allocate_exact(Request::new(AllocateExactRequest {
                operation: Some(changed_binding),
                value: Some(1.into()),
            }))
            .await
            .expect_err("cancel-first request still rejects a changed binding")
            .code(),
        Code::AlreadyExists
    );
    assert_eq!(
        provider
            .allocate_exact(Request::new(AllocateExactRequest {
                operation: Some(late_operation),
                value: Some(1.into()),
            }))
            .await
            .expect_err("late exact allocation cannot undo cancellation")
            .code(),
        Code::FailedPrecondition
    );
    assert_eq!(
        recover(provider, late_identity.clone())
            .await
            .expect("late exact allocation preserves the saved cancellation")
            .state,
        Some(State::Cancelled(Empty {}))
    );
    cancel(provider, late_identity).await;

    let active_operation = operation(pool, "allocate-before-cancel");
    let active_identity = operation_identity(&active_operation);
    assert_eq!(
        recover(provider, active_identity.clone())
            .await
            .expect("observe another unknown operation")
            .state,
        Some(State::Unknown(Empty {}))
    );
    let active_request = AllocateRequest {
        operation: Some(active_operation),
        requested_value: None,
    };
    let allocation = provider
        .allocate(Request::new(active_request.clone()))
        .await
        .expect("unknown recovery does not prevent later allocation")
        .into_inner()
        .allocation
        .expect("allocation response includes its allocation");
    assert_eq!(&allocation.route_target_policy, expected_policy);
    cancel(provider, active_identity.clone()).await;
    assert_eq!(
        lookup(provider, &allocation)
            .await
            .expect_err("cancel removes the active allocation")
            .code(),
        Code::NotFound
    );

    assert_eq!(
        recover(provider, active_identity)
            .await
            .expect("recover after cancelling an active allocation")
            .state,
        Some(State::Cancelled(Empty {}))
    );
    assert_eq!(
        provider
            .allocate(Request::new(active_request))
            .await
            .expect_err("cancelled request stays cancelled")
            .code(),
        Code::FailedPrecondition
    );
    // Both automatic values must remain usable after either cancellation order,
    // including when the provider does not offer statistics.
    let mut replacements = Vec::new();
    for (request_id, value) in [("after-cancel-one", 1), ("after-cancel-two", 2)] {
        let replacement = provider
            .allocate_exact(Request::new(AllocateExactRequest {
                operation: Some(operation(pool, request_id)),
                value: Some(value.into()),
            }))
            .await
            .expect("cancelled capacity is available without relying on statistics")
            .into_inner()
            .allocation
            .expect("allocation response includes its allocation");
        assert_eq!(&replacement.route_target_policy, expected_policy);
        assert_eq!(replacement.value, Some(value.into()));
        assert_ne!(replacement.allocation_id, allocation.allocation_id);
        replacements.push(replacement);
    }
    for replacement in replacements {
        assert!(
            provider
                .release(Request::new(release_request(&replacement)))
                .await
                .expect("release value reused after cancellation")
                .into_inner()
                .applied
        );
    }
    check_stats(provider, pool, "initial", supports_stats, [0, 2, 0, 2]).await;
}

async fn recover(
    provider: &impl ResourcePool,
    identity: OperationIdentity,
) -> Result<OperationStatus, Status> {
    let response = provider
        .recover(Request::new(RecoverRequest {
            operation_identity: Some(identity.clone()),
        }))
        .await?
        .into_inner();
    assert_eq!(
        response.operation_identity,
        Some(identity),
        "recover response identifies the requested operation"
    );
    Ok(response
        .operation_status
        .expect("recover response includes its operation status"))
}

async fn cancel(provider: &impl ResourcePool, identity: OperationIdentity) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let response = provider
            .cancel(Request::new(CancelRequest {
                operation_identity: Some(identity.clone()),
            }))
            .await
            .expect("cancel allocation operation")
            .into_inner();
        assert_eq!(
            response.operation_identity.as_ref(),
            Some(&identity),
            "cancel response identifies the requested operation"
        );
        let mut state = response
            .operation_status
            .expect("cancel response includes its operation status")
            .state;
        loop {
            match state {
                Some(State::Cancelled(_)) => return,
                Some(State::Pending(_)) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    state = recover(provider, identity.clone())
                        .await
                        .expect("recover pending cancellation")
                        .state;
                }
                other => panic!("cancellation must finish without an allocation: {other:?}"),
            }
        }
    })
    .await
    .expect("cancellation completed within five seconds");
}

async fn growth_preserves_prior_definitions(
    provider: &impl ResourcePool,
    initial: DefineRequest,
    expected_policy: &Option<RouteTargetPolicy>,
    supports_stats: bool,
) {
    provider
        .define(Request::new(initial.clone()))
        .await
        .expect("replay initial definition");
    let pool = initial.pool.as_ref().expect("pool");
    let original_request = AllocateExactRequest {
        operation: Some(operation(pool, "before-growth")),
        value: Some(3.into()),
    };
    let original_allocation = provider
        .allocate_exact(Request::new(original_request.clone()))
        .await
        .expect("keep an old-revision allocation active during growth")
        .into_inner()
        .allocation
        .expect("allocation response includes its allocation");
    assert_eq!(&original_allocation.route_target_policy, expected_policy);
    assert_eq!(original_allocation.value, Some(3.into()));
    assert_eq!(
        original_allocation.assignment_mode,
        AssignmentMode::Manual as i32
    );
    let mut changed_mode = initial.clone();
    changed_mode.eligibility[0].assignment_mode = AssignmentMode::Manual as i32;
    let mut reordered = initial.clone();
    reordered.eligibility.swap(0, 1);
    let mut changed_binding = initial.clone();
    changed_binding.pool.as_mut().expect("pool").logical_pool = "another-pool".into();
    for (scenario, changed) in [
        ("revision assignment modes cannot change", changed_mode),
        ("revision declaration order cannot change", reordered),
        ("revision pool binding cannot change", changed_binding),
    ] {
        assert_eq!(
            provider
                .define(Request::new(changed))
                .await
                .expect_err(scenario)
                .code(),
            Code::AlreadyExists,
            "{scenario}"
        );
    }
    let mut grown = DefineRequest {
        eligibility_revision: "grown".into(),
        previous_revision: Some("initial".into()),
        ..initial.clone()
    };
    // An overlapping declaration cannot move value 3 out of its manual partition.
    for value in [3, 5, 6] {
        grown.eligibility.push(Eligibility {
            assignment_mode: AssignmentMode::Automatic as i32,
            entries: Some(Entries::Value(value.into())),
        });
    }
    let mut nonadditive = grown.clone();
    nonadditive.eligibility_revision = "nonadditive".into();
    nonadditive.eligibility.remove(0);
    let missing_predecessor = DefineRequest {
        eligibility_revision: "missing-predecessor".into(),
        previous_revision: None,
        ..grown.clone()
    };
    let mut changed_pool = grown.clone();
    changed_pool.pool.as_mut().expect("pool").logical_pool = "another-pool".into();
    for (scenario, invalid, code) in [
        (
            "growth changes the saved pool binding",
            changed_pool,
            Code::FailedPrecondition,
        ),
        (
            "growth removes an earlier declaration",
            nonadditive,
            Code::FailedPrecondition,
        ),
        (
            "growth omits its predecessor",
            missing_predecessor,
            Code::FailedPrecondition,
        ),
        (
            "growth rejects a NUL in the new revision",
            DefineRequest {
                eligibility_revision: "grown\0revision".into(),
                ..grown.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "growth rejects a NUL in the predecessor revision",
            DefineRequest {
                previous_revision: Some("initial\0revision".into()),
                ..grown.clone()
            },
            Code::InvalidArgument,
        ),
    ] {
        assert_eq!(
            provider
                .define(Request::new(invalid))
                .await
                .expect_err(scenario)
                .code(),
            code,
            "{scenario}"
        );
    }
    provider
        .define(Request::new(grown.clone()))
        .await
        .expect("grow eligibility");
    provider
        .define(Request::new(initial.clone()))
        .await
        .expect("replay older definition after growth");
    provider
        .define(Request::new(grown.clone()))
        .await
        .expect("replay grown definition");
    assert_eq!(
        provider
            .define(Request::new(DefineRequest {
                eligibility_revision: "stale-predecessor".into(),
                ..grown
            }))
            .await
            .expect_err("growth must name the latest accepted predecessor")
            .code(),
        Code::FailedPrecondition
    );
    assert_eq!(
        lookup(provider, &original_allocation)
            .await
            .expect("lookup active allocation"),
        original_allocation
    );
    assert_eq!(
        provider
            .allocate_exact(Request::new(original_request))
            .await
            .expect("growth preserves the complete old assignment on replay")
            .into_inner()
            .allocation
            .expect("allocation response includes its allocation"),
        original_allocation
    );
    check_stats(provider, pool, "initial", supports_stats, [0, 2, 1, 1]).await;
    check_stats(provider, pool, "grown", supports_stats, [0, 3, 1, 1]).await;
    assert_eq!(
        provider
            .allocate_exact(Request::new(AllocateExactRequest {
                operation: Some(operation(pool, "old-revision-five")),
                value: Some(5.into()),
            }))
            .await
            .expect_err("growth does not change an older revision")
            .code(),
        Code::FailedPrecondition
    );
    let mut absent_operation = operation(pool, "eligible-but-absent");
    absent_operation.eligibility_revision = "grown".into();
    assert_eq!(
        provider
            .allocate_exact(Request::new(AllocateExactRequest {
                operation: Some(absent_operation),
                value: Some(6.into()),
            }))
            .await
            .expect_err("eligibility does not create provider inventory")
            .code(),
        Code::FailedPrecondition
    );
    assert!(
        provider
            .release(Request::new(release_request(&original_allocation)))
            .await
            .expect("the original handle still releases after growth")
            .into_inner()
            .applied
    );
    for (request_id, value, mode) in [
        ("grown-three", 3, AssignmentMode::Manual),
        ("grown-five", 5, AssignmentMode::Automatic),
    ] {
        let mut operation = operation(pool, request_id);
        operation.eligibility_revision = "grown".into();
        let allocation = provider
            .allocate_exact(Request::new(AllocateExactRequest {
                operation: Some(operation.clone()),
                value: Some(value.into()),
            }))
            .await
            .expect("allocate from grown definition")
            .into_inner()
            .allocation
            .expect("allocation response includes its allocation");
        assert_eq!(allocation.value, Some(value.into()));
        assert_eq!(allocation.operation, Some(operation));
        assert_eq!(allocation.assignment_mode, mode as i32);
        assert_eq!(&allocation.route_target_policy, expected_policy);
        assert!(
            provider
                .release(Request::new(release_request(&allocation)))
                .await
                .expect("release grown allocation")
                .into_inner()
                .applied
        );
    }
}

fn operation(pool: &PoolBinding, request_id: &str) -> AllocationOperation {
    AllocationOperation {
        pool: Some(pool.clone()),
        request_id: request_id.into(),
        eligibility_revision: "initial".into(),
        owner: Some(Owner {
            owner_type: "vpc".into(),
            owner_id: "conformance-vpc".into(),
            slot: "vni".into(),
            generation: 1,
        }),
    }
}

fn operation_identity(operation: &AllocationOperation) -> OperationIdentity {
    OperationIdentity {
        pool: operation.pool.clone(),
        request_id: operation.request_id.clone(),
    }
}

fn release_request(allocation: &Allocation) -> ReleaseRequest {
    let operation = allocation
        .operation
        .as_ref()
        .expect("allocation includes its original operation");
    ReleaseRequest {
        pool: operation.pool.clone(),
        allocation_id: allocation.allocation_id.clone(),
        owner: operation.owner.clone(),
    }
}

async fn lookup(
    provider: &impl ResourcePool,
    allocation: &Allocation,
) -> Result<Allocation, Status> {
    provider
        .lookup(Request::new(LookupRequest {
            pool: allocation
                .operation
                .as_ref()
                .expect("operation")
                .pool
                .clone(),
            allocation_id: allocation.allocation_id.clone(),
        }))
        .await
        .map(|response| {
            response
                .into_inner()
                .allocation
                .expect("lookup response includes its allocation")
        })
}

async fn check_stats(
    provider: &impl ResourcePool,
    pool: &PoolBinding,
    revision: &str,
    supported: bool,
    counts: [u64; 4],
) {
    let response = provider
        .stats(Request::new(StatsRequest {
            pool: Some(pool.clone()),
            eligibility_revision: revision.into(),
        }))
        .await;
    if !supported {
        assert_eq!(
            response
                .expect_err("unsupported stats must not return zero counts")
                .code(),
            Code::Unimplemented
        );
        return;
    }
    let [
        auto_assign_used,
        auto_assign_free,
        non_auto_assign_used,
        non_auto_assign_free,
    ] = counts;
    assert_eq!(
        response
            .expect("read provider statistics")
            .into_inner()
            .stats
            .expect("stats response includes pool statistics"),
        PoolStats {
            used: auto_assign_used + non_auto_assign_used,
            free: auto_assign_free + non_auto_assign_free,
            auto_assign_used,
            auto_assign_free,
            non_auto_assign_used,
            non_auto_assign_free,
        }
    );
}
