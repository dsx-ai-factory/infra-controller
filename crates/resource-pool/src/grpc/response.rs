// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_resource_pool_api::v1::operation_status::State;
use carbide_resource_pool_api::v1::value::Kind;
use carbide_resource_pool_api::v1::{
    AllocateExactRequest, AllocateExactResponse, AllocateRequest, AllocateResponse, Allocation,
    AllocationOperation, AssignmentMode, CancelRequest, CancelResponse, DescribeResponse,
    FailureCode, LookupRequest, LookupResponse, OperationIdentity, OperationStatus, PoolBinding,
    RecoverRequest, RecoverResponse, RouteTargetMode, StatsResponse, ValueDomain,
};
use model::resource_pool::OwnerType;

use super::GrpcResourcePoolError;
use super::GrpcResourcePoolError::InvalidResponse;

#[cfg(test)]
mod tests;

pub(super) fn describe(response: &DescribeResponse) -> Result<(), GrpcResourcePoolError> {
    if !is_valid_text(&response.authority_id) || !is_valid_text(&response.pool_id) {
        return Err(InvalidResponse(
            "provider and pool identities must be nonempty and contain no NUL",
        ));
    }
    match ValueDomain::try_from(response.value_domain) {
        Ok(ValueDomain::Integer | ValueDomain::IpAddress | ValueDomain::Ipv6Prefix) => Ok(()),
        _ => Err(InvalidResponse("value domain is missing or unknown")),
    }
}

pub(super) fn allocate(
    request: &AllocateRequest,
    response: &AllocateResponse,
) -> Result<(), GrpcResourcePoolError> {
    let allocation = response
        .allocation
        .as_ref()
        .ok_or(InvalidResponse("allocation is required"))?;
    validate_allocation(allocation)?;
    if allocation.operation != request.operation {
        return Err(InvalidResponse(
            "allocation operation does not match request",
        ));
    }
    let expected_mode = match request.requested_value.as_ref() {
        Some(value) => {
            if allocation.value.as_ref() != Some(value) {
                return Err(InvalidResponse(
                    "allocated value does not match requested value",
                ));
            }
            AssignmentMode::Manual
        }
        None => AssignmentMode::Automatic,
    };
    if allocation.assignment_mode != expected_mode as i32 {
        return Err(InvalidResponse("assignment mode does not match selection"));
    }
    Ok(())
}

pub(super) fn allocate_exact(
    request: &AllocateExactRequest,
    response: &AllocateExactResponse,
) -> Result<(), GrpcResourcePoolError> {
    let allocation = response
        .allocation
        .as_ref()
        .ok_or(InvalidResponse("allocation is required"))?;
    validate_allocation(allocation)?;
    if allocation.operation != request.operation {
        return Err(InvalidResponse(
            "allocation operation does not match request",
        ));
    }
    if allocation.value != request.value {
        return Err(InvalidResponse(
            "allocated value does not match exact value",
        ));
    }
    Ok(())
}

pub(super) fn recover(
    request: &RecoverRequest,
    response: &RecoverResponse,
) -> Result<(), GrpcResourcePoolError> {
    let identity = validate_echoed_identity(
        request.operation_identity.as_ref(),
        response.operation_identity.as_ref(),
    )?;
    let state = response
        .operation_status
        .as_ref()
        .and_then(|status| status.state.as_ref())
        .ok_or(InvalidResponse("operation state is required"))?;
    match state {
        State::Unknown(_) | State::Pending(_) | State::Cancelled(_) | State::Released(_) => Ok(()),
        State::Allocated(allocation) => {
            let operation = validate_allocation(allocation)?;
            if operation.pool != identity.pool || operation.request_id != identity.request_id {
                return Err(InvalidResponse(
                    "recovered allocation does not match operation identity",
                ));
            }
            Ok(())
        }
        State::Failed(failure) => {
            match FailureCode::try_from(failure.code) {
                Ok(
                    FailureCode::InvalidArgument
                    | FailureCode::ResourceExhausted
                    | FailureCode::FailedPrecondition,
                ) => {}
                _ => return Err(InvalidResponse("saved failure code is missing or unknown")),
            }
            if !is_valid_text(&failure.message) {
                return Err(InvalidResponse(
                    "saved failure message must be nonempty and contain no NUL",
                ));
            }
            Ok(())
        }
    }
}

pub(super) fn cancel(
    request: &CancelRequest,
    response: &CancelResponse,
) -> Result<(), GrpcResourcePoolError> {
    validate_echoed_identity(
        request.operation_identity.as_ref(),
        response.operation_identity.as_ref(),
    )?;
    match response.operation_status.as_ref() {
        Some(OperationStatus {
            state: Some(State::Pending(_) | State::Cancelled(_) | State::Released(_)),
        }) => Ok(()),
        _ => Err(InvalidResponse(
            "cancellation must be pending, cancelled, or released",
        )),
    }
}

pub(super) fn lookup(
    request: &LookupRequest,
    response: &LookupResponse,
) -> Result<(), GrpcResourcePoolError> {
    let allocation = response
        .allocation
        .as_ref()
        .ok_or(InvalidResponse("allocation is required"))?;
    let operation = validate_allocation(allocation)?;
    if operation.pool != request.pool || allocation.allocation_id != request.allocation_id {
        return Err(InvalidResponse(
            "allocation does not match lookup pool and handle",
        ));
    }
    Ok(())
}

pub(super) fn stats(response: &StatsResponse) -> Result<(), GrpcResourcePoolError> {
    let stats = response
        .stats
        .as_ref()
        .ok_or(InvalidResponse("pool statistics are required"))?;
    if stats
        .auto_assign_used
        .checked_add(stats.non_auto_assign_used)
        != Some(stats.used)
        || stats
            .auto_assign_free
            .checked_add(stats.non_auto_assign_free)
            != Some(stats.free)
    {
        return Err(InvalidResponse(
            "pool statistics totals do not match partitions",
        ));
    }
    Ok(())
}

fn validate_allocation(
    allocation: &Allocation,
) -> Result<&AllocationOperation, GrpcResourcePoolError> {
    let operation = allocation
        .operation
        .as_ref()
        .ok_or(InvalidResponse("allocation operation is required"))?;
    validate_pool(operation.pool.as_ref())?;
    if !is_valid_text(&operation.request_id) || !is_valid_text(&operation.eligibility_revision) {
        return Err(InvalidResponse(
            "allocation request and revision identities must be nonempty and contain no NUL",
        ));
    }
    let owner = operation
        .owner
        .as_ref()
        .ok_or(InvalidResponse("allocation owner is required"))?;
    if !is_valid_text(&owner.owner_type)
        || !is_valid_text(&owner.owner_id)
        || !is_valid_text(&owner.slot)
    {
        return Err(InvalidResponse(
            "allocation owner fields must be nonempty and contain no NUL",
        ));
    }
    if !is_valid_text(&allocation.allocation_id) {
        return Err(InvalidResponse(
            "allocation handle must be nonempty and contain no NUL",
        ));
    }
    let value = allocation
        .value
        .as_ref()
        .ok_or(InvalidResponse("allocated value is required"))?;
    // The request names an eligibility revision, but does not include its
    // domain. Check encoding here; the caller supplies the saved domain later.
    let domain = match value.kind.as_ref() {
        Some(Kind::Integer(_)) => ValueDomain::Integer,
        Some(Kind::Ipv4(_) | Kind::Ipv6(_)) => ValueDomain::IpAddress,
        Some(Kind::Ipv6Prefix(_)) => ValueDomain::Ipv6Prefix,
        None => return Err(InvalidResponse("allocated value kind is required")),
    };
    value
        .validate(domain)
        .map_err(|_| InvalidResponse("allocated value encoding is invalid"))?;
    match AssignmentMode::try_from(allocation.assignment_mode) {
        Ok(AssignmentMode::Automatic | AssignmentMode::Manual) => {}
        _ => return Err(InvalidResponse("assignment mode is missing or unknown")),
    }
    if let Some(policy) = allocation.route_target_policy.as_ref() {
        if owner.owner_type != OwnerType::Vpc.to_string() {
            return Err(InvalidResponse("route target policy requires a VPC owner"));
        }
        match RouteTargetMode::try_from(policy.mode) {
            Ok(RouteTargetMode::PreserveGenerated) => {}
            Ok(RouteTargetMode::ReplaceGenerated) => {
                if policy.imports.is_none() || policy.exports.is_none() {
                    return Err(InvalidResponse(
                        "replacement route target policy requires imports and exports",
                    ));
                }
            }
            Err(_) => return Err(InvalidResponse("route target mode is unknown")),
        }
    }
    Ok(operation)
}

fn validate_echoed_identity<'a>(
    expected: Option<&OperationIdentity>,
    echoed: Option<&'a OperationIdentity>,
) -> Result<&'a OperationIdentity, GrpcResourcePoolError> {
    let echoed = echoed.ok_or(InvalidResponse("operation identity is required"))?;
    validate_pool(echoed.pool.as_ref())?;
    if !is_valid_text(&echoed.request_id) {
        return Err(InvalidResponse(
            "operation request identity must be nonempty and contain no NUL",
        ));
    }
    if Some(echoed) != expected {
        return Err(InvalidResponse("operation identity does not match request"));
    }
    Ok(echoed)
}

fn validate_pool(pool: Option<&PoolBinding>) -> Result<(), GrpcResourcePoolError> {
    let pool = pool.ok_or(InvalidResponse("pool binding is required"))?;
    if !is_valid_text(&pool.site_id)
        || !is_valid_text(&pool.authority_id)
        || !is_valid_text(&pool.pool_id)
        || !is_valid_text(&pool.binding_id)
        || !is_valid_text(&pool.logical_pool)
    {
        return Err(InvalidResponse(
            "pool binding identities must be nonempty and contain no NUL",
        ));
    }
    Ok(())
}

// Provider identities and diagnostics use the same text contract. Do not trim
// whitespace or otherwise change the provider's spelling.
fn is_valid_text(value: &str) -> bool {
    !value.is_empty() && !value.contains('\0')
}
