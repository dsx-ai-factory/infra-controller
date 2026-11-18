/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    Dpu, DpuStatus, DriverOutcome, HostPrivilegeLevel, NicMode, OpCx, PlatformError, Quirk,
    RshimState,
};
use nv_redfish::core::action::{Action, ActionTarget};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::oem::nvidia::computer_system::Mode;
use serde_json::{Value, json};

use crate::dpu::nvidia::support::{
    bios_error_reports_nic_mode, bios_host_privilege_level, bios_nic_mode, bmc_rshim_state,
    enable_bmc_rshim, host_rshim_state, nic_mode_value, oem_nic_mode,
    set_bios_host_privilege_level, system_oem,
};
use crate::resources::RedfishResourcesExt as _;

/// BlueField-3: mode and host rshim are the system `Oem.Nvidia` properties
/// and actions; host privilege is a BIOS attribute.
///
/// With [`Quirk::BlueFieldOemTimeoutInNicMode`], status answers NIC mode from
/// the BIOS error before reading that resource, and mode and host rshim
/// changes post to the action targets without reading it.
pub(crate) struct BlueField3Dpu;

/// Posts `params` to the system `Oem.Nvidia` action `name`.
async fn post_oem_action<B: Bmc>(
    cx: &OpCx<'_, B>,
    name: &str,
    params: Value,
) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    let system = cx.system().await?;
    let action = Action::<Value, ()>::new(ActionTarget::new(format!(
        "{}/Oem/Nvidia/Actions/{name}",
        system.raw().odata_id
    )));
    cx.action(&action, &params).await
}

#[async_trait]
impl<B: Bmc> Dpu<B> for BlueField3Dpu
where
    B::Error: ActionError,
{
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<DpuStatus, PlatformError> {
        let bmc_rshim = bmc_rshim_state(cx).await?;
        if cx.has_quirk(Quirk::BlueFieldNicModeUnreadable) {
            return Ok(DpuStatus {
                nic_mode: None,
                host_rshim: host_rshim_state(&system_oem(cx).await?),
                bmc_rshim,
                host_privilege: bios_host_privilege_level(cx).await?,
            });
        }
        if cx.has_quirk(Quirk::BlueFieldOemTimeoutInNicMode) {
            let nic_mode_reported = cx
                .bios()
                .await
                .err()
                .is_some_and(|error| bios_error_reports_nic_mode(&error));
            if nic_mode_reported {
                return Ok(DpuStatus {
                    nic_mode: Some(NicMode::Nic),
                    host_rshim: None,
                    bmc_rshim,
                    host_privilege: None,
                });
            }
        }
        let oem = system_oem(cx).await?;
        let nic_mode = match oem_nic_mode(&oem) {
            Some(mode) => mode,
            None => bios_nic_mode(cx).await?,
        };
        Ok(DpuStatus {
            nic_mode: Some(nic_mode),
            host_rshim: host_rshim_state(&oem),
            bmc_rshim,
            host_privilege: bios_host_privilege_level(cx).await?,
        })
    }

    async fn set_nic_mode(
        &self,
        cx: &OpCx<'_, B>,
        mode: NicMode,
    ) -> Result<DriverOutcome, PlatformError> {
        if cx.has_quirk(Quirk::BlueFieldNicModeUnreadable) {
            return Err(PlatformError::Unsupported);
        }
        if cx.has_quirk(Quirk::BlueFieldOemTimeoutInNicMode) {
            return post_oem_action(cx, "Mode.Set", json!({"Mode": nic_mode_value(mode)})).await;
        }
        let mode = match mode {
            NicMode::Nic => Mode::NicMode,
            NicMode::Dpu => Mode::DpuMode,
        };
        system_oem(cx)
            .await?
            .set_mode(mode)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    async fn set_host_rshim(
        &self,
        cx: &OpCx<'_, B>,
        state: RshimState,
    ) -> Result<DriverOutcome, PlatformError> {
        if cx.has_quirk(Quirk::BlueFieldOemTimeoutInNicMode) {
            let state = match state {
                RshimState::Enabled => "Enabled",
                RshimState::Disabled => "Disabled",
            };
            return post_oem_action(cx, "HostRshim.Set", json!({"HostRshim": state})).await;
        }
        system_oem(cx)
            .await?
            .set_host_rshim(state == RshimState::Enabled)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    async fn enable_bmc_rshim(&self, cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
        enable_bmc_rshim(cx).await
    }

    async fn set_host_privilege_level(
        &self,
        cx: &OpCx<'_, B>,
        level: HostPrivilegeLevel,
    ) -> Result<DriverOutcome, PlatformError> {
        set_bios_host_privilege_level(cx, level).await
    }
}
