/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, Power};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::resource::{PowerState, ResetType};
use serde_json::json;

use crate::dell;
use crate::power::standard::StandardPower;
use crate::resources::{RedfishResourcesExt as _, attribute_map};

/// Dell iDRAC host-power behavior.
///
/// iDRAC has no AC-cycle action; the BIOS `PowerCycleRequest` job runs during
/// the next reset, so the reset issued after staging it performs the cycle.
pub(crate) struct IdracPower;

/// Refused while the BIOS still restricts UEFI variable access, which a host
/// lockdown leaves in place until the next reboot after unlocking.
async fn full_power_cycle<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    let uefi_variable_access = cx
        .bios()
        .await?
        .attribute("UefiVariableAccess")
        .and_then(|value| value.str_value().map(str::to_owned));
    if uefi_variable_access.as_deref() == Some("Controlled") {
        return Err(PlatformError::LockedDown);
    }
    let staged = dell::stage_bios_attributes(
        cx,
        &attribute_map([("PowerCycleRequest", json!("FullPowerCycle"))]),
    )
    .await?;
    let reset = match cx
        .system()
        .await?
        .power_state()
        .ok_or(PlatformError::NoContent)?
    {
        PowerState::Off => ResetType::On,
        _ => ResetType::GracefulRestart,
    };
    Ok(staged.merge(StandardPower.set(cx, reset).await?))
}

#[async_trait]
impl<B: Bmc> Power<B> for IdracPower
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
            ResetType::FullPowerCycle => full_power_cycle(cx).await,
            other => self.standard().set(cx, other).await,
        }
    }
}
