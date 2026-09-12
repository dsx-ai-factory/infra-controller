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
use std::num::NonZeroU64;
use std::str::FromStr;

use nv_redfish::core::ODataId;
use nv_redfish::resource::ResetType;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::capabilities::{LockdownDesiredState, LockdownScope};

/// Identifier returned by a vendor job service.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VendorJobId(String);

impl VendorJobId {
    pub fn new(value: String) -> Result<Self, VendorJobIdError> {
        if value.trim().is_empty() {
            return Err(VendorJobIdError);
        }
        Ok(Self(value))
    }

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

impl Serialize for VendorJobId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for VendorJobId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("vendor job id must not be empty")]
pub struct VendorJobIdError;

/// Stable machine-readable code for an operator action.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ManualInterventionCode(String);

impl ManualInterventionCode {
    pub fn new(value: String) -> Result<Self, ManualInterventionCodeError> {
        if value.trim().is_empty() {
            return Err(ManualInterventionCodeError);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ManualInterventionCode {
    type Err = ManualInterventionCodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value.to_owned())
    }
}

impl Serialize for ManualInterventionCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ManualInterventionCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("manual-intervention code must not be empty")]
pub struct ManualInterventionCodeError;

/// Persistable reference to asynchronous BMC work.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OperationReference {
    RedfishTask {
        uri: ODataId,
        retry_after_seconds: Option<u64>,
    },
    VendorJob {
        job_id: VendorJobId,
        retry_after_seconds: Option<u64>,
    },
}

impl OperationReference {
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

/// Immediate normalized result of a mutation request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "details", rename_all = "snake_case")]
pub enum DriverOutcome {
    /// The requested state is satisfied, including already-satisfied no-op mutations.
    Complete {
        follow_up: Vec<ControllerAction>,
    },
    Accepted(OperationReference),
    Blocked {
        prerequisite: ControllerAction,
        additional_prerequisites: Vec<ControllerAction>,
    },
}

impl DriverOutcome {
    pub fn complete() -> Self {
        Self::Complete {
            follow_up: Vec::new(),
        }
    }

    pub fn blocked(prerequisite: ControllerAction) -> Self {
        Self::Blocked {
            prerequisite,
            additional_prerequisites: Vec::new(),
        }
    }
}

/// Persistable orchestration requested by a capability driver.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "details", rename_all = "snake_case")]
pub enum ControllerAction {
    Power(ResetType),
    BmcReset,
    SetLockdown {
        scope: LockdownScope,
        state: LockdownDesiredState,
    },
    ClearNvram,
    RefreshExploration,
    Wait {
        seconds: NonZeroU64,
    },
    ManualIntervention {
        code: ManualInterventionCode,
    },
}

#[cfg(test)]
mod tests;
