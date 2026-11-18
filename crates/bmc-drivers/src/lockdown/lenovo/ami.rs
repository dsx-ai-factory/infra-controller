/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    DriverOutcome, Lockdown, LockdownDesiredState, LockdownScope, LockdownStatus, OpCx,
    PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::oem::ami::config_bmc::{
    ConfigBmc, ConfigBmcUpdate, LockdownBiosSettingsChangeState, LockdownBiosUpgradeDowngradeState,
    LockoutBiosVariableWriteMode, LockoutHostControlState,
};

use crate::lockdown::support::{
    RedfishLockdownExt as _, reported, signal, state_from_signals, status,
};

/// Lenovo AMI lockdown driver.
///
/// The OEM `ConfigBMC` object switches host control and BIOS protection
/// together, so `All` is the only full scope. The BMC scope toggles the host
/// interface, which status does not read; status reports the `ConfigBMC`
/// controls for both host and BMC.
pub(crate) struct LenovoAmiLockdown;

async fn config_bmc<B: Bmc>(cx: &OpCx<'_, B>) -> Result<ConfigBmc<B>, PlatformError> {
    cx.manager()
        .await?
        .oem_ami_config_bmc()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)
}

#[async_trait]
impl<B: Bmc> Lockdown<B> for LenovoAmiLockdown {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<LockdownStatus, PlatformError> {
        let config = config_bmc(cx).await?;
        let raw = config.raw();
        let signals = [
            signal(
                raw.lockout_host_control,
                LockoutHostControlState::Enable,
                LockoutHostControlState::Disable,
            ),
            signal(
                raw.lockout_bios_variable_write_mode,
                LockoutBiosVariableWriteMode::Enable,
                LockoutBiosVariableWriteMode::Disable,
            ),
            signal(
                raw.lockdown_bios_settings_change,
                LockdownBiosSettingsChangeState::Enable,
                LockdownBiosSettingsChangeState::Disable,
            ),
            signal(
                raw.lockdown_bios_upgrade_downgrade,
                LockdownBiosUpgradeDowngradeState::Enable,
                LockdownBiosUpgradeDowngradeState::Disable,
            ),
        ];
        let state = state_from_signals(&signals);
        Ok(status(
            state,
            state,
            format!(
                "host_control={}, bios_variable_write={}, bios_settings_change={}, bios_upgrade_downgrade={}",
                reported(&raw.lockout_host_control),
                reported(&raw.lockout_bios_variable_write_mode),
                reported(&raw.lockdown_bios_settings_change),
                reported(&raw.lockdown_bios_upgrade_downgrade)
            ),
        ))
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        scope: LockdownScope,
        desired: LockdownDesiredState,
    ) -> Result<DriverOutcome, PlatformError> {
        match scope {
            LockdownScope::All => {}
            LockdownScope::Bmc => {
                let enabled = desired == LockdownDesiredState::Enabled;
                return cx.set_first_host_interface(!enabled).await;
            }
            LockdownScope::Host | LockdownScope::BmcSystemLockdown => {
                return Err(PlatformError::Unsupported);
            }
        }
        let (host_control, variable_write, settings_change, upgrade_downgrade) = match desired {
            LockdownDesiredState::Enabled => (
                LockoutHostControlState::Enable,
                LockoutBiosVariableWriteMode::Enable,
                LockdownBiosSettingsChangeState::Enable,
                LockdownBiosUpgradeDowngradeState::Enable,
            ),
            LockdownDesiredState::Disabled => (
                LockoutHostControlState::Disable,
                LockoutBiosVariableWriteMode::Disable,
                LockdownBiosSettingsChangeState::Disable,
                LockdownBiosUpgradeDowngradeState::Disable,
            ),
        };
        let update = ConfigBmcUpdate::builder()
            .with_lockout_host_control(host_control)
            .with_lockout_bios_variable_write_mode(variable_write)
            .with_lockdown_bios_settings_change(settings_change)
            .with_lockdown_bios_upgrade_downgrade(upgrade_downgrade)
            .build();
        config_bmc(cx)
            .await?
            .apply(&update)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }
}
