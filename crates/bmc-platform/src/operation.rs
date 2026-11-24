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

use std::fmt;
use std::str::FromStr;

use nv_redfish::core::{ModificationResponse, ODataId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Vendor job identifier, such as an iDRAC `JID_…`; never empty.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct VendorJobId(String);

impl VendorJobId {
    /// Validates a job identifier; whitespace-only values are rejected.
    pub fn new(value: String) -> Result<Self, VendorJobIdError> {
        if value.trim().is_empty() {
            return Err(VendorJobIdError);
        }
        Ok(Self(value))
    }

    /// Returns the identifier as the BMC reported it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VendorJobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for VendorJobId {
    type Err = VendorJobIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.to_owned())
    }
}

impl TryFrom<String> for VendorJobId {
    type Error = VendorJobIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("vendor job id must not be empty")]
pub struct VendorJobIdError;

/// Persistable reference to asynchronous BMC work.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationReference {
    /// A standard Redfish task, polled at `uri`.
    RedfishTask {
        uri: ODataId,
        retry_after_seconds: Option<u64>,
        /// The Task resource the `202 Accepted` body named, read when the
        /// task monitor at `uri` is gone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<ODataId>,
    },
    /// A vendor job service entry, polled at `uri` and identified by `job_id`.
    VendorJob {
        uri: ODataId,
        job_id: VendorJobId,
        retry_after_seconds: Option<u64>,
    },
}

impl OperationReference {
    /// Resource to poll for completion.
    pub const fn uri(&self) -> &ODataId {
        match self {
            Self::RedfishTask { uri, .. } | Self::VendorJob { uri, .. } => uri,
        }
    }

    /// The Task resource a standard task's `202 Accepted` body named, if any.
    pub const fn task(&self) -> Option<&ODataId> {
        match self {
            Self::RedfishTask { task, .. } => task.as_ref(),
            Self::VendorJob { .. } => None,
        }
    }

    /// Poll interval the BMC suggested, if any.
    pub const fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::RedfishTask {
                retry_after_seconds,
                ..
            }
            | Self::VendorJob {
                retry_after_seconds,
                ..
            } => *retry_after_seconds,
        }
    }
}

/// Where asynchronous BMC work stands, read once from its [`OperationReference`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationStatus {
    /// The work is queued or executing.
    Running,
    /// The work is staged and runs on the next host reset, such as an iDRAC
    /// configuration job in `Scheduled`.
    AwaitingReset,
    Completed,
    /// The work ended without completing; `state` is the BMC's name for how it ended.
    Failed {
        state: String,
        message: Option<String>,
    },
    /// The work cannot progress until an operator acts on the BMC.
    NeedsIntervention {
        state: String,
    },
}

/// Immediate normalized result of a mutation request.
///
/// Drivers perform every step an operation needs before returning; callers
/// poll accepted work with its references and observe the effect on the
/// hardware themselves, since a BMC accepting a request does not mean the
/// host has changed state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "details", rename_all = "snake_case")]
pub enum DriverOutcome {
    /// The BMC applied the request, or the requested state already held.
    Complete,
    /// The BMC accepted asynchronous work to poll at every reference.
    Accepted {
        reference: OperationReference,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        additional_references: Vec<OperationReference>,
    },
}

impl DriverOutcome {
    /// The requested state holds.
    pub const fn complete() -> Self {
        Self::Complete
    }

    /// The BMC accepted asynchronous work to poll at `reference`.
    pub fn accepted(reference: OperationReference) -> Self {
        Self::Accepted {
            reference,
            additional_references: Vec::new(),
        }
    }

    /// Combines the outcomes of two writes that were both issued, keeping
    /// every accepted reference in issue order.
    pub fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Complete, other) => other,
            (accepted, Self::Complete) => accepted,
            (
                Self::Accepted {
                    reference,
                    mut additional_references,
                },
                Self::Accepted {
                    reference: other_reference,
                    additional_references: other_additional_references,
                },
            ) => {
                additional_references.push(other_reference);
                additional_references.extend(other_additional_references);
                Self::Accepted {
                    reference,
                    additional_references,
                }
            }
        }
    }

    /// Returns every asynchronous work reference in issue order.
    pub fn references(&self) -> impl Iterator<Item = &OperationReference> {
        let (reference, additional_references) = match self {
            Self::Accepted {
                reference,
                additional_references,
            } => (Some(reference), additional_references.as_slice()),
            Self::Complete => (None, &[][..]),
        };
        reference.into_iter().chain(additional_references)
    }
}

/// A Redfish mutation response is complete unless the BMC returned a task.
impl<T> From<ModificationResponse<T>> for DriverOutcome {
    fn from(response: ModificationResponse<T>) -> Self {
        match response {
            ModificationResponse::Entity(_)
            | ModificationResponse::Empty
            | ModificationResponse::Message(_) => Self::complete(),
            ModificationResponse::Task(task) => Self::accepted(OperationReference::RedfishTask {
                uri: task.location.0,
                retry_after_seconds: task.retry_after.map(|duration| duration.as_secs()),
                task: task.task,
            }),
        }
    }
}

#[cfg(test)]
mod tests;
