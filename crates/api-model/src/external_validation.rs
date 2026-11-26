// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Durable external-validation identities and workflow state, independent of API transport.

use carbide_uuid::external_validation::{
    ExternalValidationCycleId, ExternalValidationHoldId, ExternalValidationRequestId,
};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::StableHostMachineId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Lifecycle of NICo's allocation gate; terminal holds remain readable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum HoldState {
    /// No attempt owns this hold.
    Pending,
    /// The current attempt may allocate a validation instance.
    AttemptOpen,
    /// A terminal result awaits cleanup and local validation.
    AwaitingCleanup,
    /// Validation, cleanup, and local validation succeeded.
    Satisfied,
    /// An unresolved condition needs operator attention.
    Recovery,
}

/// Immutable result once an attempt closes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AttemptState {
    /// The attempt has no terminal result.
    Open,
    /// External validation passed, but does not itself clear the hold.
    Passed,
    /// External validation failed.
    Failed,
    /// External validation was abandoned.
    Cancelled,
}

/// Allocation progress, retained independently of instance lifetime.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AllocationState {
    /// Allocation has not been accepted, or is authoritatively fenced and absent.
    NotStarted,
    /// Accepted allocation still needs reconciliation.
    Creating,
    /// Core committed the instance and request binding.
    Created,
}

/// Distinguishes cancellation before allocation from a completed Cancelled result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ClosureKind {
    /// Cancel RPC before allocation acceptance.
    Cancel,
    /// Complete RPC for an allocated validation instance.
    Complete,
}

/// Durable audit action committed atomically with its workflow transition.
#[derive(Clone, Copy, Debug, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum AuditAction {
    /// A new attempt was opened.
    Start,
    /// An attempt was cancelled before allocation.
    Cancel,
    /// A result was recorded for a bound instance.
    Complete,
}

/// One immutable machine/cycle/tenant binding and its current allocation gate.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Hold {
    /// Stable identity of this gate.
    pub hold_id: ExternalValidationHoldId,
    /// Host whose normal allocation is gated.
    pub machine_id: StableHostMachineId,
    /// Validation tenant organization captured when NICo creates the hold.
    pub tenant_org_id: String,
    /// Discovery/reprovision cycle; never changed by an API replay.
    pub validation_cycle_id: ExternalValidationCycleId,
    /// NICo-owned health projection source, excluded only from this hold's admission check.
    pub health_report_source: String,
    /// Persisted gate lifecycle.
    pub state: HoldState,
    /// Immutable creation time and primary inventory sort key.
    pub created_at: DateTime<Utc>,
    /// Last gate transition, initially the creation time.
    pub state_changed_at: DateTime<Utc>,
    /// Current or most recent attempt; absent in Pending.
    pub current_attempt_id: Option<ExternalValidationRequestId>,
    /// Operator diagnostic present only in Recovery.
    pub recovery_reason: Option<String>,
    /// Logical removal marker; retained identities support replay but cannot admit new work.
    pub removed_at: Option<DateTime<Utc>>,
}

/// A durable operation identity and result, preserved after instance deletion.
#[derive(Clone, Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Attempt {
    /// NICo-assigned request identity.
    pub request_id: ExternalValidationRequestId,
    /// Original hold, including when a later cycle has started.
    pub hold_id: ExternalValidationHoldId,
    /// Tenant namespace of the caller's idempotency key.
    pub tenant_org_id: String,
    /// Caller-saved identity that never rebinds to another machine or cycle.
    pub caller_idempotency_key: Uuid,
    /// Open or immutable terminal result.
    pub state: AttemptState,
    /// Durable progress of the request-bound allocation.
    pub allocation_state: AllocationState,
    /// Committed instance identity retained after deletion.
    pub validation_instance_id: Option<InstanceId>,
    /// Server time at which the attempt was created.
    pub opened_at: DateTime<Utc>,
    /// Exact optional result text; omission and an empty string remain distinct.
    pub details: Option<String>,
    /// Set once on a terminal result, including pre-allocation cancellation.
    pub closed_at: Option<DateTime<Utc>>,
    /// Original closing API, used to distinguish terminal replay semantics.
    pub closure_kind: Option<ClosureKind>,
}

/// A hold and an attempt observed together in one database statement.
#[derive(Clone, Debug)]
pub struct HoldWithAttempt {
    /// Original gate and its immutable resource bindings.
    pub hold: Hold,
    /// Current attempt for inventory, or the original keyed attempt for replay.
    pub attempt: Option<Attempt>,
}

/// Exact AND-combined inventory selectors; no state selects all active holds.
#[derive(Debug, Default)]
pub struct HoldFilter {
    /// Optional organization restriction; omitted permits authorized site inventory.
    pub tenant_org_id: Option<String>,
    /// Optional exact host restriction.
    pub machine_id: Option<StableHostMachineId>,
    /// Optional exact lifecycle state, including Satisfied.
    pub state: Option<HoldState>,
}
