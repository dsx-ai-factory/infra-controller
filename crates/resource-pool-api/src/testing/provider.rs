// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::{HashMap, HashSet};
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
    pending_cancellation: bool,
    provisioned_allocations: HashSet<String>,
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
    state: SavedState,
}

enum SavedState {
    Allocated(Allocation),
    // Wire `pending` omits the allocation, but cleanup still owns its reservation.
    Cancelling(Allocation),
    Cancelled,
    Released,
    Failed(Failure),
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
                pending_cancellation: false,
                provisioned_allocations: HashSet::new(),
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

    /// `with_pending_cancellation` holds allocated requests in `pending` after
    /// `Cancel` until the test calls `complete_cleanup`.
    ///
    /// All clones share this setting. Reservations and provisioning markers
    /// remain present during cleanup, and `Release` returns `Unavailable` for
    /// those allocations. Unknown and failed requests still cancel immediately.
    pub fn with_pending_cancellation(self) -> Self {
        self.state
            .lock()
            .expect("provider fixture mutex poisoned")
            .pending_cancellation = true;
        self
    }

    /// `is_provisioned` reports whether the allocation's simulated resource
    /// exists, independently of its operation status.
    pub fn is_provisioned(&self, allocation_id: &str) -> bool {
        self.state
            .lock()
            .expect("provider fixture mutex poisoned")
            .provisioned_allocations
            .contains(allocation_id)
    }

    /// `provisioned_count` counts simulated resources independently of operation
    /// status, including resources whose allocation reply never reached a caller.
    pub fn provisioned_count(&self) -> usize {
        self.state
            .lock()
            .expect("provider fixture mutex poisoned")
            .provisioned_allocations
            .len()
    }

    /// `complete_cleanup` removes a pending cancellation's reservation and
    /// provisioned resource before making `Recover` report `cancelled`.
    ///
    /// The identity must match an operation awaiting cleanup; unknown requests
    /// return `NotFound`, and other states return `FailedPrecondition`.
    pub fn complete_cleanup(&self, identity: &OperationIdentity) -> Result<(), Status> {
        let mut state = self.state.lock().expect("provider fixture mutex poisoned");
        let key = state.operation_key(identity)?;
        let ProviderState {
            operations,
            provisioned_allocations,
            ..
        } = &mut *state;
        let saved = operations
            .get_mut(&key)
            .ok_or_else(|| Status::not_found("allocation operation does not exist"))?;
        let SavedState::Cancelling(allocation) = &saved.state else {
            return Err(Status::failed_precondition("cancellation is not pending"));
        };
        provisioned_allocations.remove(&allocation.allocation_id);
        saved.state = SavedState::Cancelled;
        Ok(())
    }

    fn allocate_request(&self, request: AllocationRequest) -> Result<Allocation, Status> {
        let mut state = self.state.lock().expect("provider fixture mutex poisoned");
        let operation = request.operation()?;
        let pool = operation
            .pool
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("operation pool is required"))?;
        pool.validate()?;
        validate_identifier(&operation.request_id, "request_id")?;
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
            Ok(allocation) => {
                state
                    .provisioned_allocations
                    .insert(allocation.allocation_id.clone());
                SavedState::Allocated(allocation.clone())
            }
            Err(error) => SavedState::Failed(Failure {
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
        validate_identifier(&request.site_id, "site_id")?;
        validate_identifier(&request.pool_alias, "pool_alias")?;
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
        validate_identifier(&request.eligibility_revision, "eligibility_revision")?;
        if let Some(previous_revision) = &request.previous_revision {
            validate_identifier(previous_revision, "previous_revision")?;
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
                    saved.state.status()
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
        let ProviderState {
            operations,
            pending_cancellation,
            provisioned_allocations,
            ..
        } = &mut *state;
        if let Some(saved) = operations.get_mut(&key) {
            match &saved.state {
                SavedState::Allocated(allocation) if *pending_cancellation => {
                    saved.state = SavedState::Cancelling(allocation.clone());
                }
                SavedState::Allocated(allocation) => {
                    provisioned_allocations.remove(&allocation.allocation_id);
                    saved.state = SavedState::Cancelled;
                }
                SavedState::Failed(_) => saved.state = SavedState::Cancelled,
                SavedState::Cancelling(_) | SavedState::Cancelled | SavedState::Released => {}
            }
            return Ok(Response::new(CancelResponse {
                operation_identity: Some(identity),
                operation_status: Some(OperationStatus {
                    state: Some(saved.state.status()),
                }),
            }));
        }
        // Remember cancellation even when an allocation request has not arrived yet.
        operations.insert(
            key,
            SavedOperation {
                identity: identity.clone(),
                request: None,
                state: SavedState::Cancelled,
            },
        );
        Ok(Response::new(CancelResponse {
            operation_identity: Some(identity),
            operation_status: Some(OperationStatus {
                state: Some(OperationState::Cancelled(Empty {})),
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
        validate_identifier(&request.allocation_id, "allocation_id")?;
        let owner = request
            .owner
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("owner is required"))?;
        owner.validate()?;
        let ProviderState {
            operations,
            provisioned_allocations,
            ..
        } = &mut *state;
        for saved in operations.values_mut() {
            if let Some(allocation) = saved.state.held_allocation()
                && allocation.allocation_id == request.allocation_id
                && allocation.operation.as_ref().is_some_and(|operation| {
                    operation.pool == request.pool && operation.owner == request.owner
                })
            {
                if matches!(saved.state, SavedState::Cancelling(_)) {
                    return Err(Status::unavailable("allocation cleanup is pending"));
                }
                provisioned_allocations.remove(&allocation.allocation_id);
                saved.state = SavedState::Released;
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
        validate_identifier(&request.allocation_id, "allocation_id")?;
        state
            .operations
            .values()
            .find_map(|saved| {
                saved.state.held_allocation().filter(|allocation| {
                    allocation.allocation_id == request.allocation_id
                        && saved.identity.pool == request.pool
                })
            })
            .map(|allocation| {
                Response::new(LookupResponse {
                    allocation: Some(allocation.clone()),
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
        validate_identifier(&identity.request_id, "request_id")?;
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
        validate_identifier(revision, "eligibility_revision")?;
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
            saved
                .state
                .held_allocation()
                .is_some_and(|allocation| allocation.value.as_ref() == Some(value))
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
            SavedState::Allocated(allocation) => Ok(allocation.clone()),
            SavedState::Failed(failure) => Err(match FailureCode::try_from(failure.code) {
                Ok(FailureCode::ResourceExhausted) => Status::resource_exhausted(&failure.message),
                Ok(FailureCode::InvalidArgument) => Status::invalid_argument(&failure.message),
                _ => Status::failed_precondition(&failure.message),
            }),
            SavedState::Cancelling(_) => Err(Status::failed_precondition(
                "allocation cancellation is pending",
            )),
            SavedState::Cancelled | SavedState::Released => Err(Status::failed_precondition(
                "allocation operation is terminal",
            )),
        }
    }
}

impl SavedState {
    fn status(&self) -> OperationState {
        match self {
            Self::Allocated(allocation) => OperationState::Allocated(allocation.clone()),
            Self::Cancelling(_) => OperationState::Pending(Empty {}),
            Self::Cancelled => OperationState::Cancelled(Empty {}),
            Self::Released => OperationState::Released(Empty {}),
            Self::Failed(failure) => OperationState::Failed(failure.clone()),
        }
    }

    fn held_allocation(&self) -> Option<&Allocation> {
        match self {
            Self::Allocated(allocation) | Self::Cancelling(allocation) => Some(allocation),
            Self::Cancelled | Self::Released | Self::Failed(_) => None,
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
            validate_identifier(value, field)?;
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
            validate_identifier(value, field)?;
        }
        Ok(())
    }
}

fn validate_identifier(value: &str, field: &str) -> Result<(), Status> {
    if value.is_empty() {
        return Err(Status::invalid_argument(format!("{field} is required")));
    }
    if value.contains('\0') {
        return Err(Status::invalid_argument(format!(
            "{field} cannot contain NUL"
        )));
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
    use tonic::{Code, Request};

    use super::LocalProvider;
    use crate::v1::eligibility::Entries;
    use crate::v1::operation_status::State;
    use crate::v1::resource_pool_server::ResourcePool;
    use crate::v1::{
        AllocateRequest, AllocationOperation, AssignmentMode, CancelRequest, DefineRequest,
        Eligibility, Empty, LookupRequest, OperationIdentity, Owner, PoolBinding, PoolStats,
        RecoverRequest, ReleaseRequest, StatsRequest, ValueDomain,
    };

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

    #[tokio::test]
    async fn pending_cancellation_keeps_inventory_and_provisioning_until_cleanup() {
        let pool = PoolBinding {
            site_id: "site".into(),
            authority_id: "provider".into(),
            pool_id: "vnis".into(),
            binding_id: "binding".into(),
            logical_pool: "vpc-vni".into(),
        };
        let provider = LocalProvider::new(pool.clone(), ValueDomain::Integer, vec![1.into()], None)
            .expect("create one available value")
            .with_pending_cancellation();
        provider
            .define(Request::new(DefineRequest {
                pool: Some(pool.clone()),
                eligibility_revision: "initial".into(),
                previous_revision: None,
                value_domain: ValueDomain::Integer as i32,
                eligibility: vec![Eligibility {
                    assignment_mode: AssignmentMode::Automatic as i32,
                    entries: Some(Entries::Value(1.into())),
                }],
            }))
            .await
            .expect("define the available value");
        let request = |request_id: &str| AllocateRequest {
            operation: Some(AllocationOperation {
                pool: Some(pool.clone()),
                request_id: request_id.into(),
                eligibility_revision: "initial".into(),
                owner: Some(Owner {
                    owner_type: "vpc".into(),
                    owner_id: "owner".into(),
                    slot: "vni".into(),
                    generation: 0,
                }),
            }),
            requested_value: None,
        };
        let allocation = provider
            .allocate(Request::new(request("original")))
            .await
            .expect("allocate before cancellation")
            .into_inner()
            .allocation
            .expect("allocated value");
        assert!(provider.is_provisioned(&allocation.allocation_id));
        let identity = OperationIdentity {
            pool: Some(pool.clone()),
            request_id: "original".into(),
        };
        let cancellation = provider
            .cancel(Request::new(CancelRequest {
                operation_identity: Some(identity.clone()),
            }))
            .await
            .expect("begin cancellation")
            .into_inner();
        assert_eq!(
            cancellation
                .operation_status
                .expect("cancellation status")
                .state,
            Some(State::Pending(Empty {}))
        );
        assert_eq!(
            provider
                .allocate(Request::new(request("original")))
                .await
                .expect_err("replay cannot restore a cancelling allocation")
                .code(),
            Code::FailedPrecondition
        );
        assert_eq!(
            provider
                .allocate(Request::new(request("competing")))
                .await
                .expect_err("cleanup still holds the only value")
                .code(),
            Code::ResourceExhausted
        );
        assert_eq!(
            provider
                .release(Request::new(ReleaseRequest {
                    pool: Some(pool.clone()),
                    allocation_id: allocation.allocation_id.clone(),
                    owner: allocation
                        .operation
                        .as_ref()
                        .expect("allocation operation")
                        .owner
                        .clone(),
                }))
                .await
                .expect_err("release cannot bypass pending cleanup")
                .code(),
            Code::Unavailable
        );
        assert!(provider.is_provisioned(&allocation.allocation_id));
        let lookup = LookupRequest {
            pool: Some(pool.clone()),
            allocation_id: allocation.allocation_id.clone(),
        };
        assert_eq!(
            provider
                .lookup(Request::new(lookup.clone()))
                .await
                .expect("lookup still returns the allocation during cleanup")
                .into_inner()
                .allocation,
            Some(allocation.clone())
        );
        assert_eq!(
            provider
                .stats(Request::new(StatsRequest {
                    pool: Some(pool.clone()),
                    eligibility_revision: "initial".into(),
                }))
                .await
                .expect("count the allocation during cleanup")
                .into_inner()
                .stats,
            Some(PoolStats {
                auto_assign_used: 1,
                used: 1,
                ..PoolStats::default()
            })
        );

        provider
            .complete_cleanup(&identity)
            .expect("finish cleanup");
        assert!(!provider.is_provisioned(&allocation.allocation_id));
        assert_eq!(
            provider
                .lookup(Request::new(lookup))
                .await
                .expect_err("completed cleanup removes the allocation")
                .code(),
            Code::NotFound
        );
        assert_eq!(
            provider
                .recover(Request::new(RecoverRequest {
                    operation_identity: Some(identity),
                }))
                .await
                .expect("recover completed cleanup")
                .into_inner()
                .operation_status
                .expect("cleanup status")
                .state,
            Some(State::Cancelled(Empty {}))
        );
        let replacement = provider
            .allocate(Request::new(request("replacement")))
            .await
            .expect("reuse capacity after cleanup")
            .into_inner()
            .allocation
            .expect("replacement allocation");
        assert_eq!(replacement.value, allocation.value);
        assert_ne!(replacement.allocation_id, allocation.allocation_id);
        assert!(provider.is_provisioned(&replacement.allocation_id));
    }
}
