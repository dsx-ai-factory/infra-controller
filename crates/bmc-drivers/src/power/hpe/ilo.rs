/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, Power};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::resource::ResetType;

use crate::power::standard::StandardPower;
use crate::power::support::RedfishPowerExt as _;

/// HPE iLO power behavior.
///
/// iLO maps `ForceRestart` to a graceful restart, and its auxiliary power
/// cycle is only accepted while the host is off, so the host is forced off
/// first.
pub(crate) struct IloPower;

/// Posts the iLO auxiliary power cycle, which the BMC accepts only while the host is off.
async fn aux_power_cycle<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    cx.system()
        .await?
        .oem_hpe_actions()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .aux_power_cycle()
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

#[async_trait]
impl<B: Bmc> Power<B> for IloPower
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Power<B> {
        &StandardPower
    }

    async fn ac_power_cycle_supported(&self, _cx: &OpCx<'_, B>) -> Result<bool, PlatformError> {
        Ok(true)
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        match reset_type {
            ResetType::ForceRestart => self.standard().set(cx, ResetType::GracefulRestart).await,
            ResetType::FullPowerCycle => {
                cx.force_off_and_wait().await?;
                aux_power_cycle(cx).await
            }
            other => self.standard().set(cx, other).await,
        }
    }
}
