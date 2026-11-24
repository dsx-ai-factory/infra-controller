/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, Power};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::resource::ResetType;

use crate::power::standard::StandardPower;

/// Supermicro power behavior: AC power is restored through the OEM system
/// reset, on systems that advertise it; SYS-221GE-FNB-NC24B does not.
pub(crate) struct SmcPower;

/// Restores AC power through the Supermicro OEM system reset.
async fn ac_power_cycle<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    cx.system()
        .await?
        .oem_supermicro_actions()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .ac_power_cycle()
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

#[async_trait]
impl<B: Bmc> Power<B> for SmcPower
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Power<B> {
        &StandardPower
    }

    async fn ac_power_cycle_supported(&self, cx: &OpCx<'_, B>) -> Result<bool, PlatformError> {
        Ok(cx
            .system()
            .await?
            .oem_supermicro_actions()
            .map_err(|error| cx.map_redfish_error(error))?
            .is_some())
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        match reset_type {
            ResetType::FullPowerCycle => ac_power_cycle(cx).await,
            other => self.standard().set(cx, other).await,
        }
    }
}
