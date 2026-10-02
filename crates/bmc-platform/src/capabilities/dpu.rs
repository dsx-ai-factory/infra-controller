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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NicMode {
    Nic,
    Dpu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostPrivilegeLevel {
    Privileged,
    Restricted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RshimState {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DpuStatus {
    pub nic_mode: Option<NicMode>,
    pub host_rshim: Option<RshimState>,
}

/// DPU NIC-mode and RShim operations.
#[async_trait]
pub trait Dpu<B: Bmc>: Send + Sync {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<DpuStatus, PlatformError>;

    async fn set_nic_mode(
        &self,
        cx: &OpCx<'_, B>,
        mode: NicMode,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn set_host_rshim(
        &self,
        cx: &OpCx<'_, B>,
        state: RshimState,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn enable_bmc_rshim(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>;

    async fn set_host_privilege_level(
        &self,
        cx: &OpCx<'_, B>,
        level: HostPrivilegeLevel,
    ) -> Result<DriverOutcome, PlatformError>;
}
