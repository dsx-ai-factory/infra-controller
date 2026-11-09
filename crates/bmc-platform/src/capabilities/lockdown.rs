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
    /// Host-side controls only (KCS, USB, in-band interfaces).
    Host,
    /// The BMC-side lock a platform toggles around configuration changes:
    /// Dell's BMC lockdown, Supermicro's system lockdown, the AMI host
    /// interface.
    Bmc,
    /// Dell iDRAC's system-lockdown switch alone, without the other BMC
    /// restrictions. Other platforms do not support it.
    BmcSystemLockdown,
    /// The platform's full lockdown.
    All,
}

/// Desired state for a lockdown mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockdownDesiredState {
    Enabled,
    Disabled,
}

/// Host and BMC lockdown state with the raw signals behind them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LockdownStatus {
    /// `Enabled`/`Disabled` once the platform's lockdown is fully applied or
    /// lifted; otherwise `Partial`. Usually that means host and BMC agree, but a
    /// platform whose lockdown leaves some host control open reports the
    /// controls its lockdown sets.
    pub aggregate: LockdownState,
    /// Human-readable list of the controls that were read.
    pub message: String,
    /// Host-side controls (KCS, USB, in-band interfaces).
    pub host: LockdownState,
    /// BMC-side controls (host interface, system lockdown switch).
    pub bmc: LockdownState,
}

/// Host and BMC lockdown status and mutation operations.
///
/// [`PlatformError::Unsupported`] means the platform has no such control:
/// from [`Self::status`], no lockdown at all; from [`Self::set`], nothing for
/// that [`LockdownScope`]. Callers skip the step rather than fail. Platforms
/// with no lockdown select no driver, so callers never reach this trait.
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
