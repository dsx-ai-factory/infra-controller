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
use nv_redfish::oem::hpe::manager::HpeManager;
use serde_json::json;

use crate::lockdown::support::{reported, signal, state_from_signals, status};
use crate::resources::{RedfishResourcesExt as _, attribute_map};

/// HPE iLO lockdown driver: KCS and USB boot for the host, the virtual NIC
/// for the BMC.
///
/// KCS is written only from iLO 6 firmware 1.40 onward. A full lockdown
/// finds the virtual NIC control before writing the host controls, so an iLO
/// without it is left as it was rather than half locked.
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

/// The iLO manager extension that reports the virtual NIC control;
/// `Unsupported` when it does not.
async fn virtual_nic_control<B: Bmc>(cx: &OpCx<'_, B>) -> Result<HpeManager<B>, PlatformError> {
    cx.manager()
        .await?
        .oem_hpe()
        .map_err(|error| cx.map_redfish_error(error))?
        .filter(|hpe| hpe.virtual_nic_enabled().is_some())
        .ok_or(PlatformError::Unsupported)
}

async fn set_bmc<B: Bmc>(cx: &OpCx<'_, B>, enabled: bool) -> Result<DriverOutcome, PlatformError> {
    virtual_nic_control(cx)
        .await?
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
                virtual_nic_control(cx).await?;
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
    use crate::test_support::{Fixture, path};

    const MANAGER: &str = "/redfish/v1/Managers/1";
    const BIOS_SETTINGS: &str = "/redfish/v1/Systems/1/Bios/Settings";

    /// A full lockdown writes nothing when iLO lacks the virtual NIC control,
    /// rather than staging the host half and then reporting it unsupported.
    #[tokio::test]
    async fn full_lockdown_writes_nothing_without_the_virtual_nic_control() {
        for (scenario, virtual_nic, expected) in [
            (
                "virtual NIC reported",
                Some(true),
                Ok(vec![BIOS_SETTINGS, MANAGER]),
            ),
            ("virtual NIC unreported", None, Err(())),
        ] {
            let mut hpe = json!({"@odata.type": "#HpeiLO.v2_11_0.HpeiLO"});
            if let Some(enabled) = virtual_nic {
                hpe["VirtualNICEnabled"] = json!(enabled);
            }
            let bmc = Fixture::new("HPE", "", "1", "1")
                .document(
                    MANAGER,
                    json!({"@odata.id": MANAGER, "Id": "1", "Name": "Manager",
                           "Oem": {"Hpe": hpe}}),
                )
                .bios("1", json!({"UsbBoot": "Enabled"}), json!({}))
                .build()
                .await;
            let cx = bmc.cx().await;

            let result = IloLockdown
                .set(&cx, LockdownScope::All, LockdownDesiredState::Enabled)
                .await;
            let writes = bmc.writes();
            let written: Vec<&str> = writes.iter().map(path).collect();
            match expected {
                Ok(expected) => {
                    result.unwrap_or_else(|error| panic!("{scenario}: {error}"));
                    assert_eq!(written, expected, "{scenario}");
                }
                Err(()) => {
                    assert!(
                        matches!(result, Err(PlatformError::Unsupported)),
                        "{scenario}: {result:?}"
                    );
                    assert!(written.is_empty(), "{scenario}: {written:?}");
                }
            }
        }
    }

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
