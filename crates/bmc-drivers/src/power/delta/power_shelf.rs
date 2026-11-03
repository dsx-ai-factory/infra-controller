/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, Power};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::resource::{PowerState, ResetType};

use crate::power::standard::StandardPower;
use crate::power::support::{RedfishPowerExt as _, power_state_from_supplies};

/// Delta power-shelf power behavior.
///
/// The shelf has no ComputerSystem; its PSUs are switched through OEM actions
/// on the PowerShelf resource and only support on and off.
pub(crate) struct DeltaPowerShelfPower;

async fn set_psus<B: Bmc>(cx: &OpCx<'_, B>, on: bool) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    let delta = cx
        .service_root()
        .power_equipment()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .power_shelves()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .members()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .into_iter()
        .next()
        .ok_or(PlatformError::Unsupported)?
        .oem_delta()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?;
    if on {
        delta.turn_on_psus().await
    } else {
        delta.turn_off_psus().await
    }
    .map(DriverOutcome::from)
    .map_err(|error| cx.map_redfish_error(error))
}

#[async_trait]
impl<B: Bmc> Power<B> for DeltaPowerShelfPower
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Power<B> {
        &StandardPower
    }

    async fn state(&self, cx: &OpCx<'_, B>) -> Result<Option<PowerState>, PlatformError> {
        let mut states = Vec::new();
        for supply in cx.power_supplies().await? {
            states.push(
                supply
                    .oem_delta()
                    .map_err(|error| cx.map_redfish_error(error))?
                    .and_then(|delta| delta.power()),
            );
        }
        Ok(power_state_from_supplies(&states))
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        match reset_type {
            ResetType::On => set_psus(cx, true).await,
            ResetType::ForceOff | ResetType::GracefulShutdown => set_psus(cx, false).await,
            _ => Err(PlatformError::Unsupported),
        }
    }
}
