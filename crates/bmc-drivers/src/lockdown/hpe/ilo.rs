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
use serde_json::json;

use crate::lockdown::support::{reported, signal, state_from_signals, status};
use crate::resources::{RedfishResourcesExt as _, attribute_map};

/// HPE iLO lockdown driver: KCS and USB boot for the host, the virtual NIC
/// for the BMC.
///
/// KCS is written only from iLO 6 firmware 1.40 onward.
pub(crate) struct IloLockdown;

/// Whether the manager firmware, reported as `iLO <major> v<minor>`, accepts
/// the KCS setting. The minor version is compared as a decimal number.
fn kcs_writable(firmware: Option<&str>) -> bool {
    let parts: Vec<&str> = firmware.unwrap_or_default().split_whitespace().collect();
    let major: i32 = parts
        .get(1)
        .and_then(|major| major.parse().ok())
        .unwrap_or_default();
    let minor: f32 = parts
        .get(2)
        .and_then(|minor| minor.get(1..))
        .and_then(|minor| minor.parse().ok())
        .unwrap_or(0.0);
    major >= 6 && minor >= 1.40
}

async fn set_kcs<B: Bmc>(cx: &OpCx<'_, B>, enabled: bool) -> Result<DriverOutcome, PlatformError> {
    let manager = cx.manager().await?;
    let firmware = manager.raw().firmware_version.clone().flatten();
    if !kcs_writable(firmware.as_deref()) {
        return Ok(DriverOutcome::complete());
    }
    let written = manager
        .network_protocol()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .oem_hpe()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .set_kcs_enabled(enabled)
        .await
        .map_err(|error| cx.map_redfish_error(error))?;
    Ok(written.map_or_else(DriverOutcome::complete, DriverOutcome::from))
}

async fn set_host<B: Bmc>(cx: &OpCx<'_, B>, enabled: bool) -> Result<DriverOutcome, PlatformError> {
    let kcs = set_kcs(cx, !enabled).await?;
    let usb_boot_value = if enabled { "Disabled" } else { "Enabled" };
    let usb_boot = cx
        .stage_bios_attributes(&attribute_map([("UsbBoot", json!(usb_boot_value))]))
        .await?;
    Ok(kcs.merge(usb_boot))
}

async fn set_bmc<B: Bmc>(cx: &OpCx<'_, B>, enabled: bool) -> Result<DriverOutcome, PlatformError> {
    cx.manager()
        .await?
        .oem_hpe()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .set_virtual_nic_enabled(!enabled)
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .map(DriverOutcome::from)
        .ok_or(PlatformError::Unsupported)
}

#[async_trait]
impl<B: Bmc> Lockdown<B> for IloLockdown {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<LockdownStatus, PlatformError> {
        let usb_boot = cx
            .bios()
            .await?
            .attribute("UsbBoot")
            .and_then(|value| value.str_value().map(str::to_owned));
        let host = state_from_signals(&[signal(usb_boot.as_deref(), "Disabled", "Enabled")]);

        let virtual_nic = cx
            .manager()
            .await?
            .oem_hpe()
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .virtual_nic_enabled();
        let bmc = state_from_signals(&[signal(virtual_nic, false, true)]);
        Ok(status(
            host,
            bmc,
            format!(
                "usb_boot={}, virtual_nic_enabled={}",
                reported(&usb_boot),
                reported(&virtual_nic)
            ),
        ))
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        scope: LockdownScope,
        desired: LockdownDesiredState,
    ) -> Result<DriverOutcome, PlatformError> {
        let enabled = desired == LockdownDesiredState::Enabled;
        match scope {
            LockdownScope::Host => set_host(cx, enabled).await,
            // iLO has no BMC-side lock apart from full lockdown.
            LockdownScope::Bmc | LockdownScope::BmcSystemLockdown => {
                Err(PlatformError::Unsupported)
            }
            LockdownScope::All => {
                let host = set_host(cx, enabled).await?;
                Ok(host.merge(set_bmc(cx, enabled).await?))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn kcs_is_written_from_ilo6_firmware_1_40() {
        value_scenarios!(kcs_writable:
            "supported" {
                Some("iLO 6 v1.40") => true,
                Some("iLO 6 v1.58") => true,
            }
            "unsupported or unparseable" {
                Some("iLO 6 v1.30") => false,
                Some("iLO 5 v2.72") => false,
                Some("iLO 7 v1.10") => false,
                Some("iLO") => false,
                None => false,
            }
        );
    }
}
