/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    DriverOutcome, Lockdown, LockdownDesiredState, LockdownScope, LockdownStatus, OpCx,
    PlatformError,
};
use nv_redfish::computer_system::{Bios, BiosUpdate};
use nv_redfish::core::Bmc;
use nv_redfish::ethernet_interface::{EthernetInterface, EthernetInterfaceUpdate};
use nv_redfish::manager::Manager;
use nv_redfish::oem::lenovo::computer_system::{
    FpMode, LenovoComputerSystem, LenovoSystemPropertiesUpdate, PortSwitchingTo,
    UsbManagementPortAssignmentUpdate,
};
use nv_redfish::oem::lenovo::manager::{KcsState, LenovoManager};
use nv_redfish::oem::lenovo::security_service::{FwRollbackState, LenovoSecurityService};
use nv_redfish::oem::lenovo::{LenovoBios, LenovoBiosUpdate, LenovoBiosUpdateExt as _};

use crate::lockdown::support::{Signal, reported, signal, state_from_signals};
use crate::resources::RedfishResourcesExt as _;

/// The manager Ethernet interface XCC exposes to the host OS.
const HOST_INTERFACE_ID: &str = "ToHost";

/// Lenovo XClarity Controller lockdown driver.
///
/// Host lockdown disables KCS, firmware rollback, and the host-facing
/// Ethernet interface, and dedicates the front-panel USB port to the server.
/// BMC lockdown prohibits firmware updates and BIOS settings changes. XCC 3
/// firmware from 2.97 reports these last two controls, and each is used only
/// when reported, so earlier firmware has no BMC-side lock. `UEFIVariablesRO`
/// is left as the BMC has it: read-only UEFI variables stop a tenant OS
/// installer from registering its boot entry. The host controls are found
/// before the first write of a change, so an XCC missing one is left as it
/// was rather than half locked.
pub(crate) struct XccLockdown;

async fn host_interface<B: Bmc>(
    cx: &OpCx<'_, B>,
    manager: &Manager<B>,
) -> Result<Option<EthernetInterface<B>>, PlatformError> {
    Ok(manager
        .ethernet_interfaces()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .members()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .into_iter()
        .find(|interface| interface.raw().id == HOST_INTERFACE_ID))
}

async fn security<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<Option<LenovoSecurityService<B>>, PlatformError> {
    cx.manager()
        .await?
        .oem_lenovo()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .security()
        .await
        .map_err(|error| cx.map_redfish_error(error))
}

/// The current BIOS and its Lenovo extension, when the system reports both.
async fn lenovo_bios<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<Option<(Bios<B>, LenovoBios)>, PlatformError> {
    let bios = match cx.bios().await {
        Ok(bios) => bios,
        Err(PlatformError::Unsupported) => return Ok(None),
        Err(error) => return Err(error),
    };
    let lenovo = bios
        .oem_lenovo()
        .map_err(|error| cx.map_redfish_error(error))?;
    Ok(lenovo.map(|lenovo| (bios, lenovo)))
}

/// Writes the Lenovo BIOS controls in `update` to the current BIOS resource.
async fn update_bios<B: Bmc>(
    cx: &OpCx<'_, B>,
    bios: &Bios<B>,
    update: LenovoBiosUpdate,
) -> Result<DriverOutcome, PlatformError> {
    let update = BiosUpdate::builder()
        .build()
        .with_oem_lenovo(update)
        .map_err(|error| PlatformError::InvalidResponse {
            message: format!("Lenovo BIOS update does not serialize: {error}"),
        })?;
    bios.update(&update)
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

fn lenovo_manager<B: Bmc>(
    cx: &OpCx<'_, B>,
    manager: &Manager<B>,
) -> Result<LenovoManager<B>, PlatformError> {
    manager
        .oem_lenovo()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)
}

/// The resources the host lockdown writes. Each carries the ETag its update
/// is conditioned on, so they are found again after other writes.
struct HostControls<B: Bmc> {
    lenovo: LenovoManager<B>,
    security: LenovoSecurityService<B>,
    to_host: EthernetInterface<B>,
    lenovo_system: LenovoComputerSystem<B>,
    front_panel: LenovoSystemPropertiesUpdate,
}

/// XCC 2 changes the manager's ETag on its own within seconds, so the
/// manager the KCS write is conditioned on is read last, just before it.
async fn find_host_controls<B: Bmc>(
    cx: &OpCx<'_, B>,
    enabled: bool,
) -> Result<HostControls<B>, PlatformError> {
    let manager = cx.manager().await?;
    let security = lenovo_manager(cx, manager)?
        .security()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?;
    let to_host = host_interface(cx, manager)
        .await?
        .ok_or(PlatformError::Unsupported)?;
    let lenovo_system = cx
        .system()
        .await?
        .oem_lenovo()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?;
    let front_panel = front_panel_update(&lenovo_system, enabled)?;
    let fresh = cx.fresh();
    let lenovo = lenovo_manager(cx, fresh.manager().await?)?;
    Ok(HostControls {
        lenovo,
        security,
        to_host,
        lenovo_system,
        front_panel,
    })
}

async fn set_host<B: Bmc>(cx: &OpCx<'_, B>, enabled: bool) -> Result<DriverOutcome, PlatformError> {
    let controls = find_host_controls(cx, enabled).await?;
    let kcs = controls
        .lenovo
        .set_kcs_enabled(!enabled)
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .map(DriverOutcome::from)
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: "XCC manager does not report KCSEnabled".to_string(),
        })?;

    let rollback = controls
        .security
        .set_fw_rollback(if enabled {
            FwRollbackState::Disabled
        } else {
            FwRollbackState::Enabled
        })
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))?;

    let body = EthernetInterfaceUpdate::builder()
        .with_interface_enabled(!enabled)
        .build();
    let to_host = controls
        .to_host
        .update(&body)
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))?;

    let front_panel = controls
        .lenovo_system
        .update(&controls.front_panel)
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))?;
    Ok(kcs.merge(rollback).merge(to_host).merge(front_panel))
}

/// The update that dedicates the front-panel USB port to the server, or
/// shares it.
///
/// Newer XCC renamed `FrontPanelUSB` to `USBManagementPortAssignment`;
/// `FrontPanelUSB` is written whenever the system reports it.
fn front_panel_update<B: Bmc>(
    lenovo: &LenovoComputerSystem<B>,
    enabled: bool,
) -> Result<LenovoSystemPropertiesUpdate, PlatformError> {
    let assignment = UsbManagementPortAssignmentUpdate::builder()
        .with_fp_mode(if enabled {
            FpMode::Server
        } else {
            FpMode::Shared
        })
        .with_port_switching_to(PortSwitchingTo::Server)
        .build();
    let raw = lenovo.raw();
    let update = if raw.front_panel_usb.is_some() {
        LenovoSystemPropertiesUpdate::builder().with_front_panel_usb(assignment)
    } else if raw.usb_management_port_assignment.is_some() {
        LenovoSystemPropertiesUpdate::builder().with_usb_management_port_assignment(assignment)
    } else {
        return Err(PlatformError::InvalidResponse {
            message: "XCC system reports neither FrontPanelUSB nor USBManagementPortAssignment"
                .to_string(),
        });
    };
    Ok(update.build())
}

/// Prohibits, or allows, firmware updates and BIOS settings changes.
///
/// Firmware updates are allowed first and prohibited last, so the firmware
/// lock is never on while the settings lock changes. Unsupported when the
/// firmware reports neither control.
async fn set_bmc<B: Bmc>(cx: &OpCx<'_, B>, enabled: bool) -> Result<DriverOutcome, PlatformError> {
    let security = security(cx).await?;
    let firmware = security
        .as_ref()
        .and_then(|security| security.fw_lockdown());
    let bios = lenovo_bios(cx).await?;
    let settings = bios
        .as_ref()
        .and_then(|(_, lenovo)| lenovo.settings_lockdown());
    if firmware.is_none() && settings.is_none() {
        return Err(PlatformError::Unsupported);
    }

    let set_firmware = async || match &security {
        Some(security) if firmware.is_some_and(|firmware| firmware != enabled) => security
            .set_fw_lockdown(enabled)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error)),
        _ => Ok(DriverOutcome::complete()),
    };
    let mut outcome = DriverOutcome::complete();
    if !enabled {
        outcome = outcome.merge(set_firmware().await?);
    }
    if let Some((bios, _)) = &bios
        && settings.is_some_and(|settings| settings != enabled)
    {
        let update = LenovoBiosUpdate::builder()
            .with_settings_lockdown(enabled)
            .build();
        outcome = outcome.merge(update_bios(cx, bios, update).await?);
    }
    if enabled {
        outcome = outcome.merge(set_firmware().await?);
    }
    Ok(outcome)
}

/// A boolean control's signal, where `true` is locked.
fn locked(value: bool) -> Signal {
    signal(Some(value), true, false)
}

#[async_trait]
impl<B: Bmc> Lockdown<B> for XccLockdown {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<LockdownStatus, PlatformError> {
        let manager = cx.manager().await?;
        let lenovo = manager
            .oem_lenovo()
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?;
        let kcs = lenovo.kcs_enabled();
        let security = lenovo
            .security()
            .await
            .map_err(|error| cx.map_redfish_error(error))?;
        let rollback = security
            .as_ref()
            .and_then(|security| security.fw_rollback());
        let firmware = security
            .as_ref()
            .and_then(|security| security.fw_lockdown());
        let to_host = host_interface(cx, manager)
            .await?
            .and_then(|interface| interface.interface_enabled());

        let lenovo_system = cx
            .system()
            .await?
            .oem_lenovo()
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?;
        // Read the assignment `set_front_panel` writes: `FrontPanelUSB` whenever reported.
        let raw = lenovo_system.raw();
        let assignment = raw
            .front_panel_usb
            .as_ref()
            .and_then(Option::as_ref)
            .or_else(|| {
                raw.usb_management_port_assignment
                    .as_ref()
                    .and_then(Option::as_ref)
            });
        let fp_mode = assignment.and_then(|value| value.fp_mode).flatten();
        let switching = assignment
            .and_then(|value| value.port_switching_to)
            .flatten();

        let lenovo_bios = lenovo_bios(cx).await?.map(|(_, lenovo)| lenovo);
        let settings = lenovo_bios
            .as_ref()
            .and_then(|lenovo| lenovo.settings_lockdown());

        let host = vec![
            signal(kcs, KcsState::Disabled, KcsState::Enabled),
            signal(
                rollback,
                FwRollbackState::Disabled,
                FwRollbackState::Enabled,
            ),
            signal(to_host, false, true),
            (
                fp_mode == Some(FpMode::Server),
                fp_mode == Some(FpMode::Shared) && switching == Some(PortSwitchingTo::Server),
            ),
        ];
        let bmc: Vec<Signal> = [firmware, settings]
            .into_iter()
            .flatten()
            .map(locked)
            .collect();
        Ok(LockdownStatus {
            aggregate: state_from_signals(&[host.as_slice(), bmc.as_slice()].concat()),
            message: format!(
                "kcs={}, firmware_rollback={}, to_host={}, front_panel={}/{}, \
                 fw_lockdown={}, settings_lockdown={}",
                reported(&kcs),
                reported(&rollback),
                reported(&to_host),
                reported(&fp_mode),
                reported(&switching),
                reported(&firmware),
                reported(&settings)
            ),
            host: state_from_signals(&host),
            bmc: state_from_signals(&bmc),
        })
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
            LockdownScope::Bmc => set_bmc(cx, enabled).await,
            LockdownScope::BmcSystemLockdown => Err(PlatformError::Unsupported),
            // The BMC locks go on after, and come off before, the host controls.
            LockdownScope::All => {
                let bmc = async || match set_bmc(cx, enabled).await {
                    Err(PlatformError::Unsupported) => Ok(DriverOutcome::complete()),
                    outcome => outcome,
                };
                if enabled {
                    let host = set_host(cx, enabled).await?;
                    Ok(host.merge(bmc().await?))
                } else {
                    find_host_controls(cx, enabled).await?;
                    let bmc = bmc().await?;
                    Ok(bmc.merge(set_host(cx, enabled).await?))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bmc_platform::LockdownState;
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{Fixture, FixtureBmc, body, path};

    const MANAGER: &str = "/redfish/v1/Managers/1";
    const SECURITY: &str = "/redfish/v1/Managers/1/Oem/Lenovo/Security";
    const INTERFACES: &str = "/redfish/v1/Managers/1/EthernetInterfaces";
    const TO_HOST: &str = "/redfish/v1/Managers/1/EthernetInterfaces/ToHost";
    const SYSTEM: &str = "/redfish/v1/Systems/1";
    const BIOS: &str = "/redfish/v1/Systems/1/Bios";

    /// The controls an XCC reports: the legacy ones, all locked or all
    /// unlocked, and the XCC 3 2.97 ones when the firmware reports them as
    /// `(FWLockdown, SettingsLockdown, UEFIVariablesRO)`.
    struct Controls {
        legacy_locked: bool,
        xcc3: Option<(bool, bool, bool)>,
    }

    /// An SR650 V4 BMC as captured from XCC 3, reduced to the lockdown
    /// resources.
    async fn xcc(controls: Controls) -> FixtureBmc {
        xcc_fixture(controls).build().await
    }

    fn xcc_fixture(controls: Controls) -> Fixture {
        let locked = controls.legacy_locked;
        let state = |locked| if locked { "Disabled" } else { "Enabled" };
        let mut security = json!({
            "@odata.id": SECURITY,
            "@odata.type": "#LenovoSecurityService.v1_0_0.LenovoSecurityService",
            "Id": "Security", "Name": "Security",
            "Configurator": {"FWRollback": state(locked)},
        });
        let mut lenovo_bios = json!({"@odata.type": "#LenovoBios.v1_0_0.LenovoBios"});
        if let Some((firmware, settings, uefi_variables)) = controls.xcc3 {
            security["FWLockdown"] = json!(firmware);
            lenovo_bios["SettingsLockdown"] = json!(settings);
            lenovo_bios["UEFIVariablesRO"] = json!(uefi_variables);
        }
        Fixture::new("Lenovo", "", "1", "1")
            .document(
                MANAGER,
                json!({
                    "@odata.id": MANAGER, "Id": "1", "Name": "Manager",
                    "EthernetInterfaces": {"@odata.id": INTERFACES},
                    "Oem": {"Lenovo": {
                        "@odata.type": "#LenovoManager.v1_0_0.LenovoManagerProperties",
                        "KCSEnabled": state(locked),
                        "Security": {"@odata.id": SECURITY},
                    }},
                }),
            )
            .document(SECURITY, security)
            .document(
                INTERFACES,
                json!({
                    "@odata.id": INTERFACES,
                    "@odata.type": "#EthernetInterfaceCollection.EthernetInterfaceCollection",
                    "Name": "Ethernet Interfaces",
                    "Members": [{"@odata.id": TO_HOST}],
                }),
            )
            .document(
                TO_HOST,
                json!({"@odata.id": TO_HOST, "Id": "ToHost", "Name": "ToHost",
                       "InterfaceEnabled": !locked}),
            )
            .document(
                SYSTEM,
                json!({
                    "@odata.id": SYSTEM, "Id": "1", "Name": "System",
                    "Bios": {"@odata.id": BIOS},
                    "Oem": {"Lenovo": {
                        "@odata.type": "#LenovoComputerSystem.v1_0_0.LenovoSystemProperties",
                        "USBManagementPortAssignment": {
                            "FPMode": if locked { "Server" } else { "Shared" },
                            "PortSwitchingTo": "Server",
                        },
                    }},
                }),
            )
            .document(
                BIOS,
                json!({"@odata.id": BIOS, "Id": "Bios", "Name": "Bios", "Attributes": {},
                       "Oem": {"Lenovo": lenovo_bios}}),
            )
    }

    /// Without the host-facing interface a full lockdown or unlock writes
    /// nothing, rather than changing some controls and reporting the rest
    /// unsupported; the unlock would otherwise lift the BMC locks first.
    #[tokio::test]
    async fn a_missing_host_control_leaves_every_control_unwritten() {
        for (scenario, locked, desired) in [
            ("full lockdown", false, LockdownDesiredState::Enabled),
            ("full unlock", true, LockdownDesiredState::Disabled),
        ] {
            let bmc = xcc_fixture(Controls {
                legacy_locked: locked,
                xcc3: Some((locked, locked, locked)),
            })
            .document(
                INTERFACES,
                json!({
                    "@odata.id": INTERFACES,
                    "@odata.type": "#EthernetInterfaceCollection.EthernetInterfaceCollection",
                    "Name": "Ethernet Interfaces",
                    "Members": [],
                }),
            )
            .build()
            .await;
            let cx = bmc.cx().await;

            let result = XccLockdown.set(&cx, LockdownScope::All, desired).await;

            assert!(
                matches!(result, Err(PlatformError::Unsupported)),
                "{scenario}: {result:?}"
            );
            assert!(bmc.writes().is_empty(), "{scenario}");
        }
    }

    /// Each scope writes only its controls, skips controls the firmware does
    /// not report, keeps firmware updates locked while the other locks change,
    /// and never writes `UEFIVariablesRO`. Legacy writes are checked by
    /// resource; new controls also by body.
    #[tokio::test]
    async fn set_writes_each_scope_in_lock_order() {
        let current = |locked| Controls {
            legacy_locked: locked,
            xcc3: Some((locked, locked, locked)),
        };
        let before_2_97 = || Controls {
            legacy_locked: false,
            xcc3: None,
        };
        let settings = |locked| Some(json!({"Oem": {"Lenovo": {"SettingsLockdown": locked}}}));
        let firmware = |locked| Some(json!({"FWLockdown": locked}));
        let legacy = [
            (MANAGER, None),
            (SECURITY, None),
            (TO_HOST, None),
            (SYSTEM, None),
        ];
        for (scenario, controls, scope, desired, expected) in [
            (
                "full lockdown",
                current(false),
                LockdownScope::All,
                LockdownDesiredState::Enabled,
                Ok([
                    legacy.to_vec(),
                    vec![(BIOS, settings(true)), (SECURITY, firmware(true))],
                ]
                .concat()),
            ),
            (
                "full unlock",
                current(true),
                LockdownScope::All,
                LockdownDesiredState::Disabled,
                Ok([
                    vec![(SECURITY, firmware(false)), (BIOS, settings(false))],
                    legacy.to_vec(),
                ]
                .concat()),
            ),
            (
                "BMC unlock",
                current(true),
                LockdownScope::Bmc,
                LockdownDesiredState::Disabled,
                Ok(vec![(SECURITY, firmware(false)), (BIOS, settings(false))]),
            ),
            (
                "BMC lockdown before 2.97",
                before_2_97(),
                LockdownScope::Bmc,
                LockdownDesiredState::Enabled,
                Err(()),
            ),
            (
                "full lockdown before 2.97",
                before_2_97(),
                LockdownScope::All,
                LockdownDesiredState::Enabled,
                Ok(legacy.to_vec()),
            ),
        ] {
            let bmc = xcc(controls).await;
            let cx = bmc.cx().await;

            let result = XccLockdown.set(&cx, scope, desired).await;
            let writes = bmc.writes();
            match expected {
                Err(()) => {
                    assert!(
                        matches!(result, Err(PlatformError::Unsupported)),
                        "{scenario}: {result:?}"
                    );
                    assert!(writes.is_empty(), "{scenario}");
                }
                Ok(expected) => {
                    result.unwrap_or_else(|error| panic!("{scenario}: {error}"));
                    let actual: Vec<(&str, Option<Value>)> = writes
                        .iter()
                        .zip(&expected)
                        .map(|(write, (_, checked))| {
                            (path(write), checked.as_ref().map(|_| body(write)))
                        })
                        .collect();
                    assert_eq!(writes.len(), expected.len(), "{scenario}: {actual:?}");
                    assert_eq!(actual, expected, "{scenario}");
                }
            }
        }
    }

    /// Status covers every lockdown control the firmware reports: the XCC 3
    /// controls count for the BMC and are left out where unreported, and
    /// `UEFIVariablesRO` never counts.
    #[tokio::test]
    async fn status_aggregates_the_reported_controls() {
        for (scenario, controls, expected) in [
            (
                "fully locked",
                Controls {
                    legacy_locked: true,
                    xcc3: Some((true, true, true)),
                },
                (
                    LockdownState::Enabled,
                    LockdownState::Enabled,
                    LockdownState::Enabled,
                ),
            ),
            (
                "locked with UEFI variables writable",
                Controls {
                    legacy_locked: true,
                    xcc3: Some((true, true, false)),
                },
                (
                    LockdownState::Enabled,
                    LockdownState::Enabled,
                    LockdownState::Enabled,
                ),
            ),
            (
                "BMC unlocked, UEFI variables read-only",
                Controls {
                    legacy_locked: true,
                    xcc3: Some((false, false, true)),
                },
                (
                    LockdownState::Partial,
                    LockdownState::Enabled,
                    LockdownState::Disabled,
                ),
            ),
            (
                "before 2.97",
                Controls {
                    legacy_locked: true,
                    xcc3: None,
                },
                (
                    LockdownState::Enabled,
                    LockdownState::Enabled,
                    LockdownState::Unknown,
                ),
            ),
        ] {
            let bmc = xcc(controls).await;
            let cx = bmc.cx().await;

            let status = XccLockdown
                .status(&cx)
                .await
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                (status.aggregate, status.host, status.bmc),
                expected,
                "{scenario}: {}",
                status.message
            );
        }
    }
}
