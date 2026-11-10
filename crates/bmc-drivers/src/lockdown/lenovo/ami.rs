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
/// Full lockdown controls OEM `ConfigBMC` protection and the host interface.
/// BMC-only lockdown controls only the host interface.
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
        let (bmc, interface) = cx.host_interface_state().await?;
        Ok(status(
            state_from_signals(&signals),
            bmc,
            format!(
                "host_control={}, bios_variable_write={}, bios_settings_change={}, bios_upgrade_downgrade={}, host_interface={}",
                reported(&raw.lockout_host_control),
                reported(&raw.lockout_bios_variable_write_mode),
                reported(&raw.lockdown_bios_settings_change),
                reported(&raw.lockdown_bios_upgrade_downgrade),
                reported(&interface)
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
            LockdownScope::All => {}
            LockdownScope::Bmc => {
                return cx.set_first_host_interface(!enabled).await;
            }
            LockdownScope::Host | LockdownScope::BmcSystemLockdown => {
                return Err(PlatformError::Unsupported);
            }
        }
        cx.require_host_interface().await?;
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
        let outcome = config_bmc(cx)
            .await?
            .apply(&update)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))?;
        Ok(outcome.merge(cx.set_first_host_interface(!enabled).await?))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};
    use bmc_platform::LockdownState;
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const MANAGER: &str = "/redfish/v1/Managers/Self";
    const CONFIG: &str = "/redfish/v1/Managers/Self/Oem/ConfigBMC";
    const INTERFACES: &str = "/redfish/v1/Managers/Self/HostInterfaces";
    const INTERFACE: &str = "/redfish/v1/Managers/Self/HostInterfaces/Self";

    fn controls(value: &str) -> Value {
        json!({
            "LockoutHostControl": value,
            "LockoutBiosVariableWriteMode": value,
            "LockdownBiosSettingsChange": value,
            "LockdownBiosUpgradeDowngrade": value,
        })
    }

    fn fixture(value: &str, enabled: Value) -> Fixture {
        let mut config = controls(value);
        config["@odata.id"] = json!(CONFIG);
        Fixture::new("Lenovo", "", "Self", "Self")
            .document(
                MANAGER,
                json!({
                    "@odata.id": MANAGER, "Id": "Self", "Name": "Manager",
                    "Oem": {"Ami": {}, "ConfigBMC": CONFIG},
                    "HostInterfaces": {"@odata.id": INTERFACES},
                }),
            )
            .document(CONFIG, config)
            .document(
                INTERFACES,
                json!({
                    "@odata.id": INTERFACES,
                    "@odata.type": "#HostInterfaceCollection.HostInterfaceCollection",
                    "Name": "Host Interfaces", "Members": [{"@odata.id": INTERFACE}],
                }),
            )
            .document(
                INTERFACE,
                json!({
                    "@odata.id": INTERFACE, "Id": "Self", "Name": "Host Interface",
                    "InterfaceEnabled": enabled,
                }),
            )
    }

    #[tokio::test]
    async fn full_and_bmc_only_lockdown_set_the_host_interface() {
        for scope in [LockdownScope::All, LockdownScope::Bmc] {
            for (desired, value, enabled) in [
                (LockdownDesiredState::Enabled, "Enable", false),
                (LockdownDesiredState::Disabled, "Disable", true),
            ] {
                let bmc = fixture("Disable", json!(true)).build().await;
                let cx = bmc.cx().await;
                assert_eq!(
                    LenovoAmiLockdown.set(&cx, scope, desired).await.unwrap(),
                    DriverOutcome::Complete
                );
                let writes = bmc.writes();
                let paths: Vec<_> = writes.iter().map(path).collect();
                if scope == LockdownScope::All {
                    assert_eq!(paths, [CONFIG, INTERFACE]);
                    assert_eq!(writes[0].method, Method::POST);
                    assert_eq!(body(&writes[0]), controls(value));
                } else {
                    assert_eq!(paths, [INTERFACE]);
                }
                let interface = writes.last().unwrap();
                assert_eq!(interface.method, Method::PATCH);
                assert_eq!(body(interface), json!({"InterfaceEnabled": enabled}));
            }
        }
    }

    #[tokio::test]
    async fn status_requires_consistent_config_and_host_interface() {
        for (value, enabled, aggregate, host, bmc_state) in [
            (
                "Enable",
                false,
                LockdownState::Enabled,
                LockdownState::Enabled,
                LockdownState::Enabled,
            ),
            (
                "Disable",
                true,
                LockdownState::Disabled,
                LockdownState::Disabled,
                LockdownState::Disabled,
            ),
            (
                "Disable",
                false,
                LockdownState::Partial,
                LockdownState::Disabled,
                LockdownState::Enabled,
            ),
            (
                "Enable",
                true,
                LockdownState::Partial,
                LockdownState::Enabled,
                LockdownState::Disabled,
            ),
        ] {
            let bmc = fixture(value, json!(enabled)).build().await;
            let cx = bmc.cx().await;
            let result = LenovoAmiLockdown.status(&cx).await.unwrap();
            assert_eq!(
                (result.aggregate, result.host, result.bmc),
                (aggregate, host, bmc_state)
            );
            assert!(
                result
                    .message
                    .contains(&format!("host_interface={enabled}"))
            );
        }
    }

    #[tokio::test]
    async fn full_lockdown_writes_nothing_without_a_host_interface() {
        for desired in [
            LockdownDesiredState::Enabled,
            LockdownDesiredState::Disabled,
        ] {
            let bmc = fixture("Disable", json!(true))
                .document(
                    INTERFACES,
                    json!({
                        "@odata.id": INTERFACES,
                        "@odata.type": "#HostInterfaceCollection.HostInterfaceCollection",
                        "Name": "Host Interfaces", "Members": [],
                    }),
                )
                .build()
                .await;
            let cx = bmc.cx().await;
            assert_eq!(
                LenovoAmiLockdown
                    .set(&cx, LockdownScope::All, desired)
                    .await,
                Err(PlatformError::NoContent)
            );
            assert!(bmc.writes().is_empty());
        }
    }

    #[tokio::test]
    async fn full_lockdown_propagates_config_and_host_interface_write_errors() {
        for (method, failed_path, expected_paths) in [
            (Method::POST, CONFIG, vec![CONFIG]),
            (Method::PATCH, INTERFACE, vec![CONFIG, INTERFACE]),
        ] {
            let bmc = fixture("Enable", json!(false))
                .respond(method, failed_path, StatusCode::INTERNAL_SERVER_ERROR, None)
                .build()
                .await;
            let cx = bmc.cx().await;
            assert!(
                LenovoAmiLockdown
                    .set(&cx, LockdownScope::All, LockdownDesiredState::Disabled)
                    .await
                    .is_err()
            );
            assert_eq!(
                bmc.writes().iter().map(path).collect::<Vec<_>>(),
                expected_paths
            );
        }
    }
}
