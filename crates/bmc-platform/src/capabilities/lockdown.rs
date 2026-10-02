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

use async_trait::async_trait;
use nv_redfish::core::Bmc;
use serde::{Deserialize, Serialize};

use crate::{DriverOutcome, OpCx, PlatformError};

/// Observed lockdown state for a host or BMC.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockdownState {
    Enabled,
    Partial,
    Disabled,
    Unknown,
}

/// Portion of platform lockdown changed by an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockdownScope {
    Host,
    Bmc,
    /// The BMC's system-lockdown switch without the other BMC restrictions.
    BmcSystemLockdown,
    All,
}

/// Desired state for a lockdown mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockdownDesiredState {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LockdownStatus {
    pub aggregate: LockdownState,
    pub message: String,
    pub host: LockdownState,
    pub bmc: LockdownState,
}

/// Host and BMC lockdown status and mutation operations.
#[async_trait]
pub trait Lockdown<B: Bmc>: Send + Sync {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<LockdownStatus, PlatformError>;

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        scope: LockdownScope,
        desired: LockdownDesiredState,
    ) -> Result<DriverOutcome, PlatformError>;
}
