// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tonic::{Request, Response, Status};

use crate::v1::operation_status::State as OperationState;
use crate::v1::resource_pool_server::ResourcePool;
use crate::v1::{
    AllocateExactRequest, AllocateExactResponse, AllocateRequest, AllocateResponse, Allocation,
    AllocationOperation, AssignmentMode, CancelRequest, CancelResponse, DefineRequest,
    DefineResponse, DescribeRequest, DescribeResponse, Eligibility, Empty, Failure, FailureCode,
    LookupRequest, LookupResponse, OperationIdentity, OperationStatus, Owner, PoolBinding,
    PoolStats, RecoverRequest, RecoverResponse, ReleaseRequest, ReleaseResponse, RouteTargetMode,
    RouteTargetPolicy, StatsRequest, StatsResponse, Value, ValueDomain,
};

/// `LocalProvider` exercises the provider RPCs against a finite inventory.
///
/// Clones share allocations and request history. Each RPC completes while holding
/// one short lock, so concurrent allocate, cancel, and release calls have a
/// defined order. Everything is lost when the fixture is dropped; this is not
/// an example of production persistence or provisioning.
#[derive(Clone)]
pub struct LocalProvider {
    state: Arc<Mutex<ProviderState>>,
    supports_stats: bool,
}

/// Inventory and request history shared by every clone of this test provider.
struct ProviderState {
    pool: PoolBinding,
    domain: ValueDomain,
    inventory: Vec<Value>,
    route_target_policy: Option<RouteTargetPolicy>,
    definitions: Vec<DefineRequest>,
    operations: HashMap<(String, String), SavedOperation>,
}

/// Keep the RPC name with its inputs: changing `Allocate` to `AllocateExact` is
/// a different request even when both would select the same value.
#[derive(PartialEq)]
enum AllocationRequest {
    Allocate(AllocateRequest),
    Exact(AllocateExactRequest),
}

/// Retain the request after cleanup so a delayed replay cannot allocate again.
struct SavedOperation {
    identity: OperationIdentity,
    // `Cancel` can arrive before the allocation request and its complete inputs.
    request: Option<AllocationRequest>,
    state: OperationState,
}

impl LocalProvider {
    /// `new` creates one provider pool with known available values.
    ///
    /// `Describe` accepts the pool's `logical_pool` or `pool_id` as its alias.
    /// Other site and binding identities can use the same authority and pool.
    /// Inventory must contain unique values of `domain`; the optional route
    /// target policy is copied into every successful allocation.
    pub fn new(
        pool: PoolBinding,
        domain: ValueDomain,
        inventory: Vec<Value>,
        route_target_policy: Option<RouteTargetPolicy>,
    ) -> Result<Self, Status> {
        pool.validate()?;
        if domain == ValueDomain::Unspecified {
            return Err(Status::invalid_argument("value domain is required"));
        }
        for (index, value) in inventory.iter().enumerate() {
            value.validate(domain)?;
            if inventory[..index].contains(value) {
                return Err(Status::invalid_argument(
                    "inventory contains duplicate values",
                ));
            }
        }
        if let Some(policy) = &route_target_policy {
            let mode = RouteTargetMode::try_from(policy.mode)
                .map_err(|_| Status::invalid_argument("unknown route target mode"))?;
            if mode == RouteTargetMode::ReplaceGenerated
                && (policy.imports.is_none() || policy.exports.is_none())
            {
                return Err(Status::invalid_argument(
                    "replacement requires import and export route target lists",
                ));
            }
        }
        Ok(Self {
            state: Arc::new(Mutex::new(ProviderState {
                pool,
                domain,
                inventory,
                route_target_policy,
                definitions: Vec::new(),
                operations: HashMap::new(),
            })),
            supports_stats: true,
        })
    }

    /// `without_stats` makes this handle report the optional statistics RPC as
    /// unsupported. Allocation and request history remain available.
    pub fn without_stats(mut self) -> Self {
        self.supports_stats = false;
        self
    }

    fn allocate_request(&self, request: AllocationRequest) -> Result<Allocation, Status> {
        let mut state = self.state.lock().expect("provider fixture mutex poisoned");
        let operation = request.operation()?;
        let pool = operation
            .pool
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("operation pool is required"))?;
        pool.validate()?;
        nonempty(&operation.request_id, "request_id")?;
        let key = (pool.site_id.clone(), operation.request_id.clone());
        if let Some(saved) = state.operations.get_mut(&key) {
            if saved.identity.pool.as_ref() != Some(pool)
                || saved
                    .request
                    .as_ref()
                    .is_some_and(|prior| prior != &request)
            {
                return Err(Status::already_exists(
                    "request_id has different allocation inputs",
                ));
            }
            if saved.request.is_none() {
                saved.request = Some(request);
            }
            return saved.allocation();
        }
        state.check_pool(pool)?;
        let result = state.allocate_new(&request);
        let saved_state = match &result {
            Ok(allocation) => OperationState::Allocated(allocation.clone()),
            Err(error) => OperationState::Failed(Failure {
                code: match error.code() {
                    tonic::Code::ResourceExhausted => FailureCode::ResourceExhausted as i32,
                    tonic::Code::InvalidArgument => FailureCode::InvalidArgument as i32,
                    _ => FailureCode::FailedPrecondition as i32,
                },
                message: error.message().to_string(),
            }),
        };
        let identity = OperationIdentity {
            pool: Some(pool.clone()),
            request_id: operation.request_id.clone(),
        };
        state.operations.insert(
            key,
            SavedOperation {
                identity,
                request: Some(request),
                state: saved_state,
            },
        );
        result
    }
}

#[tonic::async_trait]
impl ResourcePool for LocalProvider {
    async fn describe(
        &self,
        request: Request<DescribeRequest>,
    ) -> Result<Response<DescribeResponse>, Status> {
        let request = request.into_inner();
        nonempty(&request.site_id, "site_id")?;
        nonempty(&request.pool_alias, "pool_alias")?;
        let state = self.state.lock().expect("provider fixture mutex poisoned");
        if request.pool_alias != state.pool.logical_pool && request.pool_alias != state.pool.pool_id
        {
            return Err(Status::not_found("provider pool alias does not exist"));
        }
        Ok(Response::new(DescribeResponse {
            authority_id: state.pool.authority_id.clone(),
            pool_id: state.pool.pool_id.clone(),
            value_domain: state.domain as i32,
            supports_stats: self.supports_stats,
        }))
    }

    async fn define(
        &self,
        request: Request<DefineRequest>,
    ) -> Result<Response<DefineResponse>, Status> {
        let request = request.into_inner();
        let mut state = self.state.lock().expect("provider fixture mutex poisoned");
        let pool = request
            .pool
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pool is required"))?;
        pool.validate()?;
        nonempty(&request.eligibility_revision, "eligibility_revision")?;
        if let Some(previous_revision) = &request.previous_revision {
            nonempty(previous_revision, "previous_revision")?;
        }
        if let Some(prior) = state.definitions.iter().find(|prior| {
            prior
                .pool
                .as_ref()
                .is_some_and(|prior_pool| prior_pool.same_binding(pool))
                && prior.eligibility_revision == request.eligibility_revision
        }) {
            if prior != &request {
                return Err(Status::already_exists(
                    "eligibility revision has different inputs",
                ));
            }
            return Ok(Response::new(DefineResponse {}));
        }
        state.check_pool(pool)?;
        if request.value_domain != state.domain as i32 {
            return Err(Status::invalid_argument(
                "definition value domain does not match the pool",
            ));
        }
        if request.eligibility.is_empty() {
            return Err(Status::invalid_argument(
                "definition requires at least one eligibility entry",
            ));
        }
        for entry in &request.eligibility {
            entry.validate(state.domain)?;
        }
        let previous = state.definitions.iter().rev().find(|prior| {
            prior
                .pool
                .as_ref()
                .is_some_and(|prior_pool| prior_pool.same_binding(pool))
        });
        if request.previous_revision.as_deref()
            != previous.map(|prior| prior.eligibility_revision.as_str())
        {
            return Err(Status::failed_precondition(
                "previous revision does not match the latest definition",
            ));
        }
        if let Some(previous) = previous {
            if previous.pool != request.pool {
                return Err(Status::failed_precondition(
                    "growth must preserve the pool binding",
                ));
            }
            if !request.eligibility.starts_with(&previous.eligibility) {
                return Err(Status::failed_precondition(
                    "growth must preserve earlier eligibility entries",
                ));
            }
        }
        state.definitions.push(request);
        Ok(Response::new(DefineResponse {}))
    }

    async fn allocate(
        &self,
        request: Request<AllocateRequest>,
    ) -> Result<Response<AllocateResponse>, Status> {
        let allocation =
            self.allocate_request(AllocationRequest::Allocate(request.into_inner()))?;
        Ok(Response::new(AllocateResponse {
            allocation: Some(allocation),
        }))
    }

    async fn allocate_exact(
        &self,
        request: Request<AllocateExactRequest>,
    ) -> Result<Response<AllocateExactResponse>, Status> {
        let allocation = self.allocate_request(AllocationRequest::Exact(request.into_inner()))?;
        Ok(Response::new(AllocateExactResponse {
            allocation: Some(allocation),
        }))
    }

    async fn recover(
        &self,
        request: Request<RecoverRequest>,
    ) -> Result<Response<RecoverResponse>, Status> {
        let identity = request
            .into_inner()
            .operation_identity
            .ok_or_else(|| Status::invalid_argument("operation_identity is required"))?;
        let state = self.state.lock().expect("provider fixture mutex poisoned");
        let key = state.operation_key(&identity)?;
        let saved = state.operations.get(&key);
        Ok(Response::new(RecoverResponse {
            operation_identity: Some(identity),
            operation_status: Some(OperationStatus {
                state: Some(saved.map_or(OperationState::Unknown(Empty {}), |saved| {
                    saved.state.clone()
                })),
            }),
        }))
    }

    async fn cancel(
        &self,
        request: Request<CancelRequest>,
    ) -> Result<Response<CancelResponse>, Status> {
        let identity = request
            .into_inner()
            .operation_identity
            .ok_or_else(|| Status::invalid_argument("operation_identity is required"))?;
        let mut state = self.state.lock().expect("provider fixture mutex poisoned");
        let key = state.operation_key(&identity)?;
        if let Some(saved) = state.operations.get_mut(&key) {
            if !matches!(saved.state, OperationState::Released(_)) {
                saved.state = OperationState::Cancelled(Empty {});
            }
            return Ok(Response::new(CancelResponse {
                operation_identity: Some(identity),
                operation_status: Some(OperationStatus {
                    state: Some(saved.state.clone()),
                }),
            }));
        }
        // Remember cancellation even when an allocation request has not arrived yet.
        let terminal = OperationState::Cancelled(Empty {});
        state.operations.insert(
            key,
            SavedOperation {
                identity: identity.clone(),
                request: None,
                state: terminal.clone(),
            },
        );
        Ok(Response::new(CancelResponse {
            operation_identity: Some(identity),
            operation_status: Some(OperationStatus {
                state: Some(terminal),
            }),
        }))
    }

    async fn release(
        &self,
        request: Request<ReleaseRequest>,
    ) -> Result<Response<ReleaseResponse>, Status> {
        let request = request.into_inner();
        let mut state = self.state.lock().expect("provider fixture mutex poisoned");
        let pool = request
            .pool
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pool is required"))?;
        state.check_pool(pool)?;
        nonempty(&request.allocation_id, "allocation_id")?;
        let owner = request
            .owner
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("owner is required"))?;
        owner.validate()?;
        for saved in state.operations.values_mut() {
            if let OperationState::Allocated(allocation) = &saved.state
                && allocation.allocation_id == request.allocation_id
                && allocation.operation.as_ref().is_some_and(|operation| {
                    operation.pool == request.pool && operation.owner == request.owner
                })
            {
                saved.state = OperationState::Released(Empty {});
                return Ok(Response::new(ReleaseResponse { applied: true }));
            }
        }
        Ok(Response::new(ReleaseResponse { applied: false }))
    }

    async fn lookup(
        &self,
        request: Request<LookupRequest>,
    ) -> Result<Response<LookupResponse>, Status> {
        let request = request.into_inner();
        let state = self.state.lock().expect("provider fixture mutex poisoned");
        let pool = request
            .pool
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pool is required"))?;
        state.check_pool(pool)?;
        nonempty(&request.allocation_id, "allocation_id")?;
        state
            .operations
            .values()
            .find_map(|saved| match &saved.state {
                OperationState::Allocated(allocation)
                    if allocation.allocation_id == request.allocation_id
                        && saved.identity.pool == request.pool =>
                {
                    Some(allocation.clone())
                }
                _ => None,
            })
            .map(|allocation| {
                Response::new(LookupResponse {
                    allocation: Some(allocation),
                })
            })
            .ok_or_else(|| Status::not_found("allocation handle is not active"))
    }

    async fn stats(
        &self,
        request: Request<StatsRequest>,
    ) -> Result<Response<StatsResponse>, Status> {
        if !self.supports_stats {
            return Err(Status::unimplemented(
                "provider statistics are not supported",
            ));
        }
        let request = request.into_inner();
        let state = self.state.lock().expect("provider fixture mutex poisoned");
        let pool = request
            .pool
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pool is required"))?;
        state.check_pool(pool)?;
        let definition = state.definition(pool, &request.eligibility_revision)?;
        let mut stats = PoolStats::default();
        for value in &state.inventory {
            let Some(mode) = assignment_mode(&definition.eligibility, value)? else {
                continue;
            };
            match (mode == AssignmentMode::Automatic, state.allocated(value)) {
                (true, true) => stats.auto_assign_used += 1,
                (true, false) => stats.auto_assign_free += 1,
                (false, true) => stats.non_auto_assign_used += 1,
                (false, false) => stats.non_auto_assign_free += 1,
            }
        }
        stats.used = stats.auto_assign_used + stats.non_auto_assign_used;
        stats.free = stats.auto_assign_free + stats.non_auto_assign_free;
        Ok(Response::new(StatsResponse { stats: Some(stats) }))
    }
}

impl ProviderState {
    /// `operation_key` checks the pool binding against any saved operation
    /// before returning the `(site_id, request_id)` key.
    fn operation_key(&self, identity: &OperationIdentity) -> Result<(String, String), Status> {
        let pool = identity
            .pool
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("pool is required"))?;
        pool.validate()?;
        nonempty(&identity.request_id, "request_id")?;
        let key = (pool.site_id.clone(), identity.request_id.clone());
        if let Some(saved) = self.operations.get(&key) {
            saved.check_identity(identity)?;
        }
        self.check_pool(pool)?;
        Ok(key)
    }

    fn check_pool(&self, pool: &PoolBinding) -> Result<(), Status> {
        pool.validate()?;
        if pool.authority_id != self.pool.authority_id || pool.pool_id != self.pool.pool_id {
            return Err(Status::not_found(
                "authority or provider pool does not match",
            ));
        }
        Ok(())
    }

    fn definition(&self, pool: &PoolBinding, revision: &str) -> Result<&DefineRequest, Status> {
        nonempty(revision, "eligibility_revision")?;
        self.definitions
            .iter()
            .find(|definition| {
                definition.pool.as_ref() == Some(pool)
                    && definition.eligibility_revision == revision
            })
            .ok_or_else(|| Status::failed_precondition("eligibility revision is not defined"))
    }

    fn allocated(&self, value: &Value) -> bool {
        self.operations.values().any(|saved| {
            matches!(&saved.state, OperationState::Allocated(allocation) if allocation.value.as_ref() == Some(value))
        })
    }

    fn allocate_new(&self, request: &AllocationRequest) -> Result<Allocation, Status> {
        let operation = request.operation()?;
        let owner = operation
            .owner
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("owner is required"))?;
        owner.validate()?;
        let pool = operation
            .pool
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("operation pool is required"))?;
        let definition = self.definition(pool, &operation.eligibility_revision)?;
        let requested = match request {
            AllocationRequest::Allocate(request) => request.requested_value.as_ref(),
            AllocationRequest::Exact(request) => {
                Some(request.value.as_ref().ok_or_else(|| {
                    Status::invalid_argument("exact allocation value is required")
                })?)
            }
        };
        if let Some(value) = requested {
            value.validate(self.domain)?;
        }
        let partition = match request {
            AllocationRequest::Allocate(_) => Some(if requested.is_some() {
                AssignmentMode::Manual
            } else {
                AssignmentMode::Automatic
            }),
            AllocationRequest::Exact(_) => None,
        };
        let mut candidates = Vec::new();
        for value in &self.inventory {
            if let Some(mode) = assignment_mode(&definition.eligibility, value)?
                && partition.is_none_or(|partition| partition == mode)
                && !self.allocated(value)
            {
                candidates.push((value, mode));
            }
        }
        // Requested create checks partition exhaustion before a particular value,
        // matching the Integrated allocator's existing error precedence.
        if partition.is_some() && candidates.is_empty() {
            return Err(Status::resource_exhausted(
                "no free values in the requested partition",
            ));
        }
        let (value, assignment_mode) = candidates
            .into_iter()
            .find(|(value, _)| requested.is_none_or(|requested| requested == *value))
            .ok_or_else(|| {
                Status::failed_precondition("requested value is unavailable or ineligible")
            })?;
        Ok(Allocation {
            operation: Some(operation.clone()),
            // Retained request history keeps these handles unique after release.
            allocation_id: format!("allocation-{}", self.operations.len()),
            value: Some(value.clone()),
            assignment_mode: assignment_mode as i32,
            route_target_policy: self.route_target_policy.clone(),
        })
    }
}

impl AllocationRequest {
    fn operation(&self) -> Result<&AllocationOperation, Status> {
        let operation = match self {
            Self::Allocate(request) => &request.operation,
            Self::Exact(request) => &request.operation,
        };
        operation
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("operation is required"))
    }
}

impl SavedOperation {
    fn check_identity(&self, identity: &OperationIdentity) -> Result<(), Status> {
        if &self.identity != identity {
            return Err(Status::already_exists(
                "request_id belongs to a different pool binding",
            ));
        }
        Ok(())
    }

    fn allocation(&self) -> Result<Allocation, Status> {
        match &self.state {
            OperationState::Allocated(allocation) => Ok(allocation.clone()),
            OperationState::Failed(failure) => Err(match FailureCode::try_from(failure.code) {
                Ok(FailureCode::ResourceExhausted) => Status::resource_exhausted(&failure.message),
                Ok(FailureCode::InvalidArgument) => Status::invalid_argument(&failure.message),
                _ => Status::failed_precondition(&failure.message),
            }),
            _ => Err(Status::failed_precondition(
                "allocation operation is terminal",
            )),
        }
    }
}

impl PoolBinding {
    fn same_binding(&self, other: &Self) -> bool {
        self.site_id == other.site_id && self.binding_id == other.binding_id
    }

    fn validate(&self) -> Result<(), Status> {
        for (value, field) in [
            (&self.site_id, "site_id"),
            (&self.authority_id, "authority_id"),
            (&self.pool_id, "pool_id"),
            (&self.binding_id, "binding_id"),
            (&self.logical_pool, "logical_pool"),
        ] {
            nonempty(value, field)?;
        }
        Ok(())
    }
}

impl Owner {
    fn validate(&self) -> Result<(), Status> {
        for (value, field) in [
            (&self.owner_type, "owner_type"),
            (&self.owner_id, "owner_id"),
            (&self.slot, "slot"),
        ] {
            nonempty(value, field)?;
        }
        Ok(())
    }
}

fn nonempty(value: &str, field: &str) -> Result<(), Status> {
    if value.is_empty() {
        return Err(Status::invalid_argument(format!("{field} is required")));
    }
    Ok(())
}

fn assignment_mode(
    eligibility: &[Eligibility],
    value: &Value,
) -> Result<Option<AssignmentMode>, Status> {
    for entry in eligibility {
        if entry.contains(value)? {
            return Ok(Some(entry.assignment_mode()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use tonic::Code;

    use super::LocalProvider;
    use crate::v1::{PoolBinding, ValueDomain};

    #[test]
    fn empty_inventory_does_not_allow_an_unspecified_domain() {
        let pool = PoolBinding {
            site_id: "site".to_string(),
            authority_id: "provider".to_string(),
            pool_id: "vnis".to_string(),
            binding_id: "binding".to_string(),
            logical_pool: "vpc-vni".to_string(),
        };
        let error = LocalProvider::new(pool, ValueDomain::Unspecified, Vec::new(), None)
            .err()
            .expect("an unspecified domain must fail even without inventory values");
        assert_eq!(error.code(), Code::InvalidArgument);
    }
}
