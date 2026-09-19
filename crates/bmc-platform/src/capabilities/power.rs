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

use std::time::SystemTime;

use async_trait::async_trait;
use nv_redfish::core::Bmc;
use nv_redfish::resource::{PowerState, ResetType};

use crate::{DriverOutcome, OpCx, PlatformError};

/// Host and chassis power observations and mutations.
#[async_trait]
pub trait Power<B: Bmc>: Send + Sync {
    async fn state(&self, cx: &OpCx<'_, B>) -> Result<PowerState, PlatformError>;

    async fn ac_power_cycle_supported(&self, cx: &OpCx<'_, B>) -> Result<bool, PlatformError>;

    /// Reports whether BMC event logs contain evidence of a restart since `since`.
    ///
    /// This is a decision-driving read used to verify force-restart requests;
    /// general log retrieval remains outside the driver platform.
    async fn restart_observed_since(
        &self,
        cx: &OpCx<'_, B>,
        since: SystemTime,
    ) -> Result<bool, PlatformError>;

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError>;

    async fn chassis_reset(
        &self,
        cx: &OpCx<'_, B>,
        chassis_id: &str,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError>;
}
