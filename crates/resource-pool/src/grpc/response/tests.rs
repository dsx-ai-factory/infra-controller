// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_resource_pool_api::v1 as wire;
use carbide_test_support::Outcome::{Fails, Yields};
use carbide_test_support::scenarios;
use wire::operation_status::State;

fn pool() -> wire::PoolBinding {
    wire::PoolBinding {
        authority_id: "authority".into(),
        pool_id: "pool".into(),
        binding_id: "binding".into(),
        logical_pool: "vpc-vni".into(),
    }
}

fn operation() -> wire::AllocationOperation {
    wire::AllocationOperation {
        pool: Some(pool()),
        request_id: "request".into(),
        owner: Some(wire::Owner {
            owner_type: "vpc".into(),
            owner_id: "owner".into(),
            slot: "vni".into(),
            generation: 0,
        }),
        eligibility_revision: "revision".into(),
    }
}

fn identity() -> wire::OperationIdentity {
    wire::OperationIdentity {
        pool: Some(pool()),
        request_id: "request".into(),
    }
}

fn allocation() -> wire::Allocation {
    wire::Allocation {
        operation: Some(operation()),
        allocation_id: "handle".into(),
        value: Some(42_i64.into()),
        assignment_mode: wire::AssignmentMode::Automatic.into(),
        route_target_policy: None,
    }
}

fn changed_allocation(change: impl FnOnce(&mut wire::Allocation)) -> wire::Allocation {
    let mut allocation = allocation();
    change(&mut allocation);
    allocation
}

fn status(state: State) -> wire::OperationStatus {
    wire::OperationStatus { state: Some(state) }
}

#[test]
fn describe_requires_identity_and_recognized_domain() {
    let valid = wire::DescribeResponse {
        authority_id: " opaque authority Δ ".into(),
        pool_id: " opaque pool 池 ".into(),
        value_domain: wire::ValueDomain::Integer.into(),
        supports_stats: false,
    };
    scenarios!(run = |response| super::describe(&response).map_err(drop);
        "opaque identities and unsupported stats are valid" {
            valid.clone() => Yields(()),
        }
        "known address and prefix domains are valid" {
            wire::DescribeResponse { value_domain: wire::ValueDomain::IpAddress.into(), ..valid.clone() } => Yields(()),
            wire::DescribeResponse { value_domain: wire::ValueDomain::Ipv6Prefix.into(), ..valid.clone() } => Yields(()),
        }
        "missing identity" {
            wire::DescribeResponse { authority_id: String::new(), ..valid.clone() } => Fails,
            wire::DescribeResponse { pool_id: String::new(), ..valid.clone() } => Fails,
        }
        "identities cannot contain NUL" {
            wire::DescribeResponse { authority_id: "authority\0id".into(), ..valid.clone() } => Fails,
            wire::DescribeResponse { pool_id: "pool\0id".into(), ..valid.clone() } => Fails,
        }
        "domain must be supplied and recognized" {
            wire::DescribeResponse { value_domain: 0, ..valid.clone() } => Fails,
            wire::DescribeResponse { value_domain: i32::MAX, ..valid } => Fails,
        }
    );
}

#[test]
fn allocation_requires_complete_assignment() {
    // Lookup supplies no expected owner or revision: these rows prove structural
    // validation rather than merely rejecting a mismatch with Allocate's request.
    let request = wire::LookupRequest {
        pool: Some(pool()),
        allocation_id: "handle".into(),
    };
    scenarios!(run = |allocation| super::lookup(&request, &wire::LookupResponse { allocation }).map_err(drop);
        "address and prefix kinds select their valid wire domain" {
            Some(changed_allocation(|a| a.value = Some(std::net::Ipv4Addr::new(192, 0, 2, 1).into()))) => Yields(()),
            Some(changed_allocation(|a| a.value = Some(std::net::Ipv6Addr::LOCALHOST.into()))) => Yields(()),
            Some(changed_allocation(|a| a.value = Some(wire::Value {
                kind: Some(wire::value::Kind::Ipv6Prefix(wire::Ipv6Prefix {
                    network: vec![0; 16],
                    prefix_len: 64,
                })),
            }))) => Yields(()),
        }
        "required assignment fields" {
            None => Fails,
            Some(changed_allocation(|a| a.operation = None)) => Fails,
            Some(changed_allocation(|a| a.allocation_id.clear())) => Fails,
            Some(changed_allocation(|a| a.value = None)) => Fails,
        }
        "required request identity and owner fields" {
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().request_id.clear())) => Fails,
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().owner.as_mut().unwrap().owner_type.clear())) => Fails,
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().owner.as_mut().unwrap().owner_id.clear())) => Fails,
        }
        "required owner and revision" {
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().owner = None)) => Fails,
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().owner.as_mut().unwrap().slot.clear())) => Fails,
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().eligibility_revision.clear())) => Fails,
        }
        "returned operation text cannot contain NUL" {
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().request_id = "request\0id".into())) => Fails,
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().owner.as_mut().unwrap().owner_id = "owner\0id".into())) => Fails,
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().eligibility_revision = "revision\0id".into())) => Fails,
        }
        "delegates malformed values to the wire validator" {
            Some(changed_allocation(|a| a.value = Some(wire::Value { kind: Some(wire::value::Kind::Ipv4(vec![127])) }))) => Fails,
        }
        "mode must be supplied and recognized" {
            Some(changed_allocation(|a| a.assignment_mode = 0)) => Fails,
            Some(changed_allocation(|a| a.assignment_mode = i32::MAX)) => Fails,
        }
    );
}

#[test]
fn allocate_correlates_complete_operation_and_selection() {
    let request = wire::AllocateRequest {
        operation: Some(operation()),
        requested_value: None,
    };
    scenarios!(run = |allocation| super::allocate(&request, &wire::AllocateResponse { allocation }).map_err(drop);
        "successful transport still requires an assignment" {
            None => Fails,
        }
        "correlation includes binding, owner generation, and revision" {
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().pool.as_mut().unwrap().binding_id = "other-binding".into())) => Fails,
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().owner.as_mut().unwrap().generation = 1)) => Fails,
            Some(changed_allocation(|a| a.operation.as_mut().unwrap().eligibility_revision = "other-revision".into())) => Fails,
        }
        "automatic requests cannot return manual assignments" {
            Some(changed_allocation(|a| a.assignment_mode = wire::AssignmentMode::Manual.into())) => Fails,
        }
    );

    let request = wire::AllocateRequest {
        requested_value: Some(42_i64.into()),
        ..request
    };
    scenarios!(run = |allocation| super::allocate(&request, &wire::AllocateResponse { allocation: Some(allocation) }).map_err(drop);
        "requested selection must retain both mode and value" {
            allocation() => Fails,
            changed_allocation(|a| {
                a.assignment_mode = wire::AssignmentMode::Manual.into();
                a.value = Some(43_i64.into());
            }) => Fails,
        }
    );
}

#[test]
fn allocate_exact_checks_value_without_forcing_manual_mode() {
    let request = wire::AllocateExactRequest {
        operation: Some(operation()),
        value: Some(42_i64.into()),
    };
    scenarios!(run = |allocation| super::allocate_exact(&request, &wire::AllocateExactResponse { allocation: Some(allocation) }).map_err(drop);
        "exact selection preserves the existing partition" {
            allocation() => Yields(()),
        }
        "returned exact value and operation must match" {
            changed_allocation(|a| a.value = Some(43_i64.into())) => Fails,
            changed_allocation(|a| a.operation.as_mut().unwrap().request_id = "other-request".into()) => Fails,
        }
    );
}

#[test]
fn route_target_policy_validates_mode_and_direction_presence() {
    let request = wire::LookupRequest {
        pool: Some(pool()),
        allocation_id: "handle".into(),
    };
    let replacement = wire::RouteTargetPolicy {
        mode: wire::RouteTargetMode::ReplaceGenerated.into(),
        imports: Some(wire::RouteTargets::default()),
        exports: Some(wire::RouteTargets::default()),
        peer_identity: vec![],
    };
    scenarios!(run = |policy| {
        let allocation = changed_allocation(|a| a.route_target_policy = Some(policy));
        super::lookup(&request, &wire::LookupResponse { allocation: Some(allocation) }).map_err(drop)
    };
        "preserve mode permits omitted directions" {
            wire::RouteTargetPolicy {
                mode: wire::RouteTargetMode::PreserveGenerated.into(),
                ..Default::default()
            } => Yields(()),
        }
        "present empty replacement directions are valid" {
            replacement.clone() => Yields(()),
        }
        "replacement requires both directions" {
            wire::RouteTargetPolicy { imports: None, ..replacement.clone() } => Fails,
            wire::RouteTargetPolicy { exports: None, ..replacement.clone() } => Fails,
        }
        "unknown policy modes cannot default to preserve" {
            wire::RouteTargetPolicy { mode: i32::MAX, ..replacement.clone() } => Fails,
        }
    );

    let allocation = changed_allocation(|a| {
        a.operation
            .as_mut()
            .unwrap()
            .owner
            .as_mut()
            .unwrap()
            .owner_type = "machine".into();
        a.route_target_policy = Some(replacement);
    });
    assert!(
        super::lookup(
            &request,
            &wire::LookupResponse {
                allocation: Some(allocation)
            }
        )
        .is_err()
    );
}

#[test]
fn recover_requires_echo_and_status() {
    let request = wire::RecoverRequest {
        operation_identity: Some(identity()),
    };
    let response = wire::RecoverResponse {
        operation_identity: Some(identity()),
        operation_status: Some(status(State::Pending(wire::Empty {}))),
    };
    scenarios!(run = |response| super::recover(&request, &response).map_err(drop);
        "pending is a valid nonterminal observation" {
            response.clone() => Yields(()),
        }
        "required echo and status" {
            wire::RecoverResponse { operation_identity: None, ..response.clone() } => Fails,
            wire::RecoverResponse { operation_status: None, ..response.clone() } => Fails,
            wire::RecoverResponse { operation_status: Some(wire::OperationStatus::default()), ..response.clone() } => Fails,
        }
        "echo must identify the queried request and binding" {
            wire::RecoverResponse {
                operation_identity: Some(wire::OperationIdentity { request_id: "other-request".into(), ..identity() }),
                ..response.clone()
            } => Fails,
            wire::RecoverResponse {
                operation_identity: Some(wire::OperationIdentity {
                    pool: Some(wire::PoolBinding { binding_id: "other-binding".into(), ..pool() }),
                    ..identity()
                }),
                ..response
            } => Fails,
        }
    );
}

#[test]
fn recover_checks_text_even_when_identity_matches() {
    let valid = wire::OperationIdentity {
        request_id: " request Δ ".into(),
        pool: Some(wire::PoolBinding {
            pool_id: " pool 池 ".into(),
            ..pool()
        }),
    };
    scenarios!(run = |identity: wire::OperationIdentity| {
        let request = wire::RecoverRequest {
            operation_identity: Some(identity.clone()),
        };
        let response = wire::RecoverResponse {
            operation_identity: Some(identity),
            operation_status: Some(status(State::Pending(wire::Empty {}))),
        };
        super::recover(&request, &response).map_err(drop)
    };
        "matching Unicode and whitespace remain valid" {
            valid.clone() => Yields(()),
        }
        "matching NUL text is still invalid" {
            wire::OperationIdentity { request_id: "request\0id".into(), ..valid.clone() } => Fails,
            wire::OperationIdentity {
                pool: Some(wire::PoolBinding { pool_id: "pool\0id".into(), ..pool() }),
                ..valid
            } => Fails,
        }
    );
}

#[test]
fn recover_validates_allocated_identity_and_saved_failure() {
    let request = wire::RecoverRequest {
        operation_identity: Some(identity()),
    };
    scenarios!(run = |state| {
        super::recover(&request, &wire::RecoverResponse {
            operation_identity: Some(identity()),
            operation_status: Some(status(state)),
        }).map_err(drop)
    };
        "a saved invalid-argument rejection is a valid observation" {
            State::Failed(wire::Failure {
                code: wire::FailureCode::InvalidArgument.into(),
                message: " malformed allocation request Δ ".into(),
            }) => Yields(()),
        }
        "saved allocation requires a handle" {
            State::Allocated(changed_allocation(|a| a.allocation_id.clear())) => Fails,
            State::Allocated(changed_allocation(|a| a.allocation_id = "allocation\0id".into())) => Fails,
        }
        "nested assignment must belong to the queried identity" {
            State::Allocated(changed_allocation(|a| a.operation.as_mut().unwrap().request_id = "other-request".into())) => Fails,
            State::Allocated(changed_allocation(|a| a.operation.as_mut().unwrap().pool.as_mut().unwrap().binding_id = "other-binding".into())) => Fails,
        }
        "failure must have a recognized definite code and diagnostic" {
            State::Failed(wire::Failure { code: 0, message: "rejected".into() }) => Fails,
            State::Failed(wire::Failure { code: i32::MAX, message: "rejected".into() }) => Fails,
            State::Failed(wire::Failure { code: wire::FailureCode::InvalidArgument.into(), message: String::new() }) => Fails,
            State::Failed(wire::Failure { code: wire::FailureCode::InvalidArgument.into(), message: "provider\0diagnostic".into() }) => Fails,
        }
    );
}

#[test]
fn cancel_requires_cleanup_state_for_the_queried_operation() {
    let request = wire::CancelRequest {
        operation_identity: Some(identity()),
    };
    scenarios!(run = |state| {
        super::cancel(&request, &wire::CancelResponse {
            operation_identity: Some(identity()),
            operation_status: Some(status(state)),
        }).map_err(drop)
    };
        "cleanup continues or has completed" {
            State::Pending(wire::Empty {}) => Yields(()),
            State::Cancelled(wire::Empty {}) => Yields(()),
            State::Released(wire::Empty {}) => Yields(()),
        }
        "other observations do not acknowledge cancellation" {
            State::Unknown(wire::Empty {}) => Fails,
            State::Allocated(allocation()) => Fails,
            State::Failed(wire::Failure { code: wire::FailureCode::ResourceExhausted.into(), message: "exhausted".into() }) => Fails,
        }
    );

    let foreign_response = wire::CancelResponse {
        operation_identity: Some(wire::OperationIdentity {
            request_id: "other-request".into(),
            ..identity()
        }),
        operation_status: Some(status(State::Cancelled(wire::Empty {}))),
    };
    assert!(super::cancel(&request, &foreign_response).is_err());
}

#[test]
fn lookup_correlates_original_binding_and_handle() {
    let request = wire::LookupRequest {
        pool: Some(pool()),
        allocation_id: "handle".into(),
    };
    scenarios!(run = |allocation| super::lookup(&request, &wire::LookupResponse { allocation: Some(allocation) }).map_err(drop);
        "lookup must return the requested allocation instance" {
            changed_allocation(|a| a.allocation_id = "other-handle".into()) => Fails,
            changed_allocation(|a| a.operation.as_mut().unwrap().pool.as_mut().unwrap().logical_pool = "other-pool".into()) => Fails,
        }
    );
}

#[test]
fn stats_requires_consistent_checked_partition_totals() {
    let valid = wire::PoolStats {
        used: 3,
        free: u64::MAX,
        auto_assign_used: 1,
        auto_assign_free: u64::MAX,
        non_auto_assign_used: 2,
        non_auto_assign_free: 0,
    };
    scenarios!(run = |stats| super::stats(&wire::StatsResponse { stats }).map_err(drop);
        "full wire range is valid when sums agree" {
            Some(valid) => Yields(()),
        }
        "absence cannot become zero inventory" {
            None => Fails,
        }
        "both totals must match their partitions" {
            Some(wire::PoolStats { used: 4, ..valid }) => Fails,
            Some(wire::PoolStats { free: 0, ..valid }) => Fails,
        }
        "partition sums must not wrap" {
            Some(wire::PoolStats { used: 0, auto_assign_used: u64::MAX, non_auto_assign_used: 1, ..valid }) => Fails,
            Some(wire::PoolStats { free: 0, non_auto_assign_free: 1, ..valid }) => Fails,
        }
    );
}
