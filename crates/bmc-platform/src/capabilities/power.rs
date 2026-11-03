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
use nv_redfish::resource::{PowerState, ResetType};

use crate::{DriverOutcome, OpCx, PlatformError};

/// Host and chassis power observations and mutations.
///
/// Every operation defaults to delegating to [`Self::standard`], so a driver
/// implements only the operations its platform deviates on.
#[async_trait]
pub trait Power<B: Bmc>: Send + Sync {
    /// The driver every operation this driver does not implement delegates to.
    ///
    /// Vendor and model drivers return the capability's standard driver and
    /// implement only their deviations. The standard driver implements every
    /// operation and returns `self`.
    fn standard(&self) -> &dyn Power<B>;

    /// The power state; `None` when the platform cannot tell, such as a power
    /// shelf whose supplies disagree or do not report.
    async fn state(&self, cx: &OpCx<'_, B>) -> Result<Option<PowerState>, PlatformError> {
        self.standard().state(cx).await
    }

    /// Whether `set` can perform [`ResetType::FullPowerCycle`] on this platform.
    async fn ac_power_cycle_supported(&self, cx: &OpCx<'_, B>) -> Result<bool, PlatformError> {
        self.standard().ac_power_cycle_supported(cx).await
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard().set(cx, reset_type).await
    }

    async fn chassis_reset(
        &self,
        cx: &OpCx<'_, B>,
        chassis_id: &str,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        self.standard()
            .chassis_reset(cx, chassis_id, reset_type)
            .await
    }
}
