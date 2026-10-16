/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Remote allocation requests, provider assignments, and cleanup progress.
//!
//! The nested types are stored as JSON in `resource_pool_operations`.
//! Their field names and tags are part of the stored format; SQL constraints and
//! updates also read the owner fields, pool identities, and allocation result.
//! Changes to these types must preserve reads and replay comparisons with records
//! written by earlier versions, including the representation of absent fields.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::OwnerType;
use super::allocation::{AssignmentMode, ResourcePoolValue};
use crate::vpc::routing_profile::RouteTargetConfig;

/// `RemotePoolIdentity` keeps retries pointed at the original issuing pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePoolIdentity {
    /// NICo site UUID; the caller must persist and share it across all replicas.
    pub site_id: Uuid,
    /// Stable provider identity returned when resolving the configured backend.
    pub authority_id: String,
    /// Stable provider pool identity, rather than its configurable lookup alias.
    pub pool_id: String,
    /// NICo's logical pool name, e.g. `vpc-vni`.
    pub logical_pool: String,
}

/// `RemoteAllocationRequest` records the immutable inputs for one attempt.
/// Repeating `request_id` must use the same fields, even after cleanup.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RemoteAllocationRequest {
    /// Site-wide request UUID, never reused within the issuing authority.
    pub request_id: Uuid,
    /// Saved relationship between NICo's logical pool and its backend.
    pub binding_id: Uuid,
    /// Original site and provider identities used for allocation and cleanup.
    #[sqlx(json)]
    pub pool: RemotePoolIdentity,
    /// Entity, purpose, and attempt generation that will hold the value.
    #[sqlx(json)]
    pub owner: AllocationOwner,
    /// Immutable effective eligibility revision accepted by the provider.
    pub eligibility_revision: String,
    /// Automatic, caller-requested, or exact allocation for this attempt.
    #[sqlx(json)]
    pub selection: AllocationSelection,
}

/// `AllocationOwner` distinguishes separate uses of a pool by the same entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocationOwner {
    /// Kind of entity holding the value, e.g. `Vpc` or `Machine`.
    pub owner_type: OwnerType,
    /// Stable entity ID in the owner's existing string format.
    pub owner_id: String,
    /// Purpose within the owner; a DPU loopback slot includes its VPC ID.
    pub slot: String,
    /// Attempt generation for this owner and slot; zero is a valid first value.
    /// The caller determines which generation is still wanted before attachment.
    pub generation: u64,
}

/// `AllocationSelection` preserves the existing allocator's three choices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "value", rename_all = "snake_case")]
pub enum AllocationSelection {
    /// Choose an available value from the automatic partition.
    Automatic,
    /// Reserve the supplied value from the manual partition.
    Requested(ResourcePoolValue),
    /// Reserve the supplied value from either partition without changing its mode.
    Exact(ResourcePoolValue),
}

/// `CompletionAuthority` records who may attach a successful allocation.
/// A saved request alone never authorizes creating or restoring an entity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionAuthority {
    /// Only this synchronous request may attach before its deadline.
    Foreground {
        /// Identity of the synchronous request that is waiting for the result.
        attempt_id: Uuid,
        /// Deadline after which this request can no longer attach its result.
        attach_before: DateTime<Utc>,
    },
    /// A controller may attach while the separately stored request is still wanted.
    /// The caller must check that record, owner generation, entity existence, and
    /// deletion state in the same transaction that attaches the allocation.
    Controller {
        /// Identity of the separately stored request the controller must check.
        desire_id: Uuid,
    },
}

/// `RemoteOperationState` records NICo's attachment and cleanup progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum RemoteOperationState {
    /// The request is saved but has not attached an allocation. A definite
    /// failure also stays here until cancellation is requested and confirmed.
    Pending,
    /// The allocation was attached in the same transaction as the entity change.
    Attached,
    /// NICo must cancel the request, including any allocation that arrives late.
    CancelRequested,
    /// NICo must release the original allocation after its entity stops using it.
    ReleaseRequested,
    /// The provider confirmed cancellation and prevents allocation replay.
    Cancelled,
    /// The provider confirmed release and prevents allocation replay.
    Released,
}

/// `CleanupConfirmation` identifies cleanup the provider has confirmed complete.
/// `NotFound`, a false release result, or an unknown request are not confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupConfirmation {
    /// The provider confirmed the request is cancelled, including owned provisioning.
    Cancelled,
    /// The provider confirmed the original assignment and provisioning are released.
    Released,
}

/// `RemoteAllocationOperation` keeps an attempt's original inputs, result,
/// and local state. Keep terminal records so delayed work cannot reuse their
/// request identities, even after cleanup removes the allocation record.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RemoteAllocationOperation {
    /// Immutable allocation inputs saved before contacting the provider.
    #[sqlx(flatten)]
    pub request: RemoteAllocationRequest,
    /// Who may attach the allocation, subject to the entity's current state.
    #[sqlx(json)]
    pub completion_authority: CompletionAuthority,
    /// NICo's attachment and cleanup progress for this attempt.
    pub state: RemoteOperationState,
    /// Confirmed assignment or rejection; `None` means neither has been saved.
    #[sqlx(json(nullable))]
    pub result: Option<AllocationResult>,
    /// Time the request was first saved.
    pub created_at: DateTime<Utc>,
    /// Time of the last applied write, including an identical result or cleanup replay.
    pub updated_at: DateTime<Utc>,
}

/// `AllocationResult` saves the provider's definite assignment or rejection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "result", rename_all = "snake_case")]
pub enum AllocationResult {
    /// Ready assignment, including any routing policy returned with the value.
    Allocated(AllocationAssignment),
    /// Definite rejection saved by the provider for this request. The owner slot
    /// remains blocked until NICo confirms cancellation.
    Failed(AllocationFailure),
}

/// `AllocationAssignment` identifies one reservation, even if its value is reused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocationAssignment {
    /// Provider handle, unique and never reused within the issuing pool.
    pub allocation_id: String,
    /// Value reserved for the request's owner, slot, and generation.
    pub value: ResourcePoolValue,
    /// Partition the value belongs to; exact allocation preserves this mode.
    pub assignment_mode: AssignmentMode,
    /// VPC routing policy returned with the assignment; `None` preserves NICo policy.
    pub route_target_policy: Option<ProviderRouteTargetPolicy>,
}

/// `AllocationFailure` preserves the provider's definite rejection for replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocationFailure {
    /// Reason the provider rejected the allocation.
    pub code: AllocationFailureCode,
    /// Provider diagnostic, excluding credentials and sensitive request contents.
    pub message: String,
}

/// `AllocationFailureCode` distinguishes the provider's terminal rejections.
/// Connectivity failures leave the result unknown and do not belong here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocationFailureCode {
    /// The allocation request is malformed.
    InvalidArgument,
    /// The selected partition has no available eligible value.
    ResourceExhausted,
    /// The requested value or eligibility revision cannot be used.
    FailedPrecondition,
}

/// `ProviderRouteTargetPolicy` saves the routing policy issued with a VPC VNI.
/// Preserve list order and duplicates when saving and replaying the assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRouteTargetPolicy {
    /// Whether the provider keeps or replaces NICo's generated route targets.
    pub mode: ProviderRouteTargetMode,
    /// Import targets; `None` preserves imports, while `Some([])` adds none.
    /// Replacement mode requires this field to be present.
    pub imports: Option<Vec<RouteTargetConfig>>,
    /// Export targets; `None` preserves exports, while `Some([])` adds none.
    /// Replacement mode requires this field to be present.
    pub exports: Option<Vec<RouteTargetConfig>>,
    /// Export targets that identify only this VPC for peering; empty means none.
    pub peer_identity: Vec<RouteTargetConfig>,
}

/// `ProviderRouteTargetMode` controls NICo's generated import and export targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRouteTargetMode {
    /// Keep generated targets and append any provider targets.
    #[serde(rename = "preserve_generated")]
    Preserve,
    /// Replace generated targets with explicit NICo opt-in; keep configured additions.
    #[serde(rename = "replace_generated")]
    Replace,
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases};
    use serde::de::DeserializeOwned;
    use serde_json::Value;

    use super::*;

    fn check_stored_json<T>(cases: impl IntoIterator<Item = (&'static str, &'static str, T)>)
    where
        T: Debug + PartialEq + Serialize + DeserializeOwned,
    {
        check_cases(
            cases.into_iter().map(|(scenario, stored, expected)| Case {
                scenario,
                input: stored,
                expect: Yields((
                    expected,
                    serde_json::from_str::<Value>(stored).expect("valid stored JSON fixture"),
                )),
            }),
            |stored| {
                let decoded: T = serde_json::from_str(stored).map_err(|error| error.to_string())?;
                let encoded = serde_json::to_value(&decoded).map_err(|error| error.to_string())?;
                Ok::<_, String>((decoded, encoded))
            },
        );
    }

    #[test]
    fn request_identity_preserves_stored_pool_and_owner_fields() {
        check_stored_json([(
            "issuing pool identity",
            r#"{"site_id":"00000000-0000-0000-0000-000000000001","authority_id":"provider-a","pool_id":"pool-7","logical_pool":"vpc-vni"}"#,
            RemotePoolIdentity {
                site_id: Uuid::from_u128(1),
                authority_id: "provider-a".to_owned(),
                pool_id: "pool-7".to_owned(),
                logical_pool: "vpc-vni".to_owned(),
            },
        )]);
        check_stored_json([(
            "owner slot and generation",
            r#"{"owner_type":"machine","owner_id":"machine-1","slot":"loopback:vpc-1","generation":7}"#,
            AllocationOwner {
                owner_type: OwnerType::Machine,
                owner_id: "machine-1".to_owned(),
                slot: "loopback:vpc-1".to_owned(),
                generation: 7,
            },
        )]);
    }

    #[test]
    fn completion_authority_preserves_stored_tags_and_deadline() {
        check_stored_json([
            (
                "foreground identity and deadline",
                r#"{"kind":"foreground","attempt_id":"00000000-0000-0000-0000-000000000002","attach_before":"2026-10-16T12:00:00Z"}"#,
                CompletionAuthority::Foreground {
                    attempt_id: Uuid::from_u128(2),
                    attach_before: "2026-10-16T12:00:00Z".parse().unwrap(),
                },
            ),
            (
                "controller desire identity",
                r#"{"kind":"controller","desire_id":"00000000-0000-0000-0000-000000000003"}"#,
                CompletionAuthority::Controller {
                    desire_id: Uuid::from_u128(3),
                },
            ),
        ]);
    }

    #[test]
    fn allocation_result_preserves_stored_status_and_assignment_fields() {
        check_stored_json([
            (
                "assignment handle and policy",
                r#"{
                    "status":"allocated",
                    "result":{
                        "allocation_id":"handle-7",
                        "value":{"kind":"integer","value":73},
                        "assignment_mode":"manual",
                        "route_target_policy":{
                            "mode":"preserve_generated",
                            "imports":null,
                            "exports":[],
                            "peer_identity":[]
                        }
                    }
                }"#,
                AllocationResult::Allocated(AllocationAssignment {
                    allocation_id: "handle-7".to_owned(),
                    value: ResourcePoolValue::Integer(73),
                    assignment_mode: AssignmentMode::Manual,
                    route_target_policy: Some(ProviderRouteTargetPolicy {
                        mode: ProviderRouteTargetMode::Preserve,
                        imports: None,
                        exports: Some(Vec::new()),
                        peer_identity: Vec::new(),
                    }),
                }),
            ),
            (
                "definite rejection",
                r#"{"status":"failed","result":{"code":"failed_precondition","message":"eligibility revision rejected"}}"#,
                AllocationResult::Failed(AllocationFailure {
                    code: AllocationFailureCode::FailedPrecondition,
                    message: "eligibility revision rejected".to_owned(),
                }),
            ),
        ]);
    }
}
