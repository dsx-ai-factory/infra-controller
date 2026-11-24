/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! BlueField DPU mechanics shared by the card generations.

use std::collections::BTreeMap;

use bmc_platform::{
    DriverOutcome, HostPrivilegeLevel, NicMode, OpCx, PlatformError, Quirk, RshimState,
};
use nv_redfish::computer_system::Bios;
use nv_redfish::core::{Bmc, ODataId};
use nv_redfish::oem::nvidia::NvidiaComputerSystem;
use nv_redfish::oem::nvidia::computer_system::{HostRshim, Mode};
use serde_json::{Value, json};

use crate::resources::{RedfishResourcesExt as _, attribute_map, bios_update};

/// BMC 24.10 dropped the spaces from BIOS attribute names; see
/// [`Quirk::BlueFieldSpacedBiosAttributeNames`].
const HOST_PRIVILEGE_LEVEL: &str = "HostPrivilegeLevel";
const HOST_PRIVILEGE_LEVEL_WITH_SPACES: &str = "Host Privilege Level";

fn parse_mode(value: &str) -> Option<NicMode> {
    match value.replace('"', "").as_str() {
        "NicMode" => Some(NicMode::Nic),
        "DpuMode" => Some(NicMode::Dpu),
        _ => None,
    }
}

pub(super) fn nic_mode_value(mode: NicMode) -> &'static str {
    match mode {
        NicMode::Nic => "NicMode",
        NicMode::Dpu => "DpuMode",
    }
}

/// Whether a failed BIOS read is the NIC-mode 500 whose body reports NIC mode.
pub(super) fn bios_error_reports_nic_mode(error: &PlatformError) -> bool {
    let PlatformError::Bmc {
        status: 500,
        message,
        ..
    } = error
    else {
        return false;
    };
    serde_json::from_str::<Value>(message)
        .ok()
        .and_then(|body| body.pointer("/Attributes/NicMode").cloned())
        .and_then(|mode| mode.as_str().and_then(parse_mode))
        == Some(NicMode::Nic)
}

fn reported_nic_mode<B: Bmc>(bios: &Bios<B>) -> Result<NicMode, PlatformError> {
    bios.attribute("NicMode")
        .and_then(|value| value.str_value().and_then(parse_mode))
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: "BlueField BIOS does not report NicMode".to_string(),
        })
}

/// The mode the system `Oem.Nvidia` resource reports; BlueField-3 falls
/// back to BIOS without one.
pub(super) fn oem_nic_mode(oem: &NvidiaComputerSystem<impl Bmc>) -> Option<NicMode> {
    match oem.mode()? {
        Mode::NicMode => Some(NicMode::Nic),
        Mode::DpuMode => Some(NicMode::Dpu),
        Mode::UnsupportedValue => None,
    }
}

pub(super) fn host_rshim_state(oem: &NvidiaComputerSystem<impl Bmc>) -> Option<RshimState> {
    match oem.host_rshim()? {
        HostRshim::Enabled => Some(RshimState::Enabled),
        HostRshim::Disabled => Some(RshimState::Disabled),
        HostRshim::UnsupportedValue => None,
    }
}

/// The BIOS resource; `None` when firmware with
/// [`Quirk::BlueFieldNicModeBiosError`] answers the read with the 500 it
/// returns in NIC mode.
async fn bios_unless_nic_mode<B: Bmc>(cx: &OpCx<'_, B>) -> Result<Option<Bios<B>>, PlatformError> {
    match cx.bios().await {
        Ok(bios) => Ok(Some(bios)),
        Err(error)
            if cx.has_quirk(Quirk::BlueFieldNicModeBiosError)
                && bios_error_reports_nic_mode(&error) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Reads `NicMode` from the BIOS attributes, reading NIC mode out of the 500
/// that firmware with [`Quirk::BlueFieldNicModeBiosError`] answers with.
pub(super) async fn bios_nic_mode<B: Bmc>(cx: &OpCx<'_, B>) -> Result<NicMode, PlatformError> {
    match bios_unless_nic_mode(cx).await? {
        Some(bios) => reported_nic_mode(&bios),
        None => Ok(NicMode::Nic),
    }
}

/// The system `Oem.Nvidia` resource.
pub(super) async fn system_oem<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<NvidiaComputerSystem<B>, PlatformError> {
    cx.system()
        .await?
        .oem_nvidia()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)
}

/// Enables the BMC side of rshim through the manager's `Oem.Nvidia` resource.
pub(super) async fn enable_bmc_rshim<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<DriverOutcome, PlatformError> {
    cx.manager()
        .await?
        .oem_nvidia()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .set_bmc_rshim_enabled(true)
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

/// The BMC side of rshim, kept on the resource the manager's `Oem.Nvidia`
/// object links; `None` when the manager has no such resource.
pub(super) async fn bmc_rshim_state<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<Option<RshimState>, PlatformError> {
    let Some(oem) = cx
        .manager()
        .await?
        .oem_nvidia()
        .map_err(|error| cx.map_redfish_error(error))?
    else {
        return Ok(None);
    };
    let enabled = oem
        .bmc_rshim_enabled()
        .await
        .map_err(|error| cx.map_redfish_error(error))?;
    Ok(enabled.map(|enabled| {
        if enabled {
            RshimState::Enabled
        } else {
            RshimState::Disabled
        }
    }))
}

/// Writes `attributes` to the BIOS pending settings. Firmware with
/// [`Quirk::BlueFieldNicModeBiosError`] cannot read the BIOS in NIC mode, so
/// there the write goes to the system's `Bios/Settings` without reading the
/// BIOS for its advertised settings link.
pub(super) async fn write_bios<B: Bmc>(
    cx: &OpCx<'_, B>,
    attributes: &BTreeMap<String, Value>,
) -> Result<DriverOutcome, PlatformError> {
    if !cx.has_quirk(Quirk::BlueFieldNicModeBiosError) {
        return cx.write_bios_attributes(attributes).await;
    }
    let bios = cx
        .system()
        .await?
        .raw()
        .bios
        .as_ref()
        .ok_or(PlatformError::Unsupported)?
        .id()
        .to_string();
    cx.bmc()
        .update::<_, Value>(
            &ODataId::from(format!("{bios}/Settings")),
            None,
            &bios_update(attributes)?,
        )
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_bmc_error(error))
}

fn host_privilege_value(level: HostPrivilegeLevel) -> &'static str {
    match level {
        HostPrivilegeLevel::Privileged => "Privileged",
        HostPrivilegeLevel::Restricted => "Restricted",
    }
}

/// Reads the host privilege level from BIOS under either spelling; `None`
/// in NIC mode on firmware with [`Quirk::BlueFieldNicModeBiosError`], which
/// cannot serve the BIOS then.
pub(super) async fn bios_host_privilege_level<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<Option<HostPrivilegeLevel>, PlatformError> {
    let Some(bios) = bios_unless_nic_mode(cx).await? else {
        return Ok(None);
    };
    let value = [HOST_PRIVILEGE_LEVEL, HOST_PRIVILEGE_LEVEL_WITH_SPACES]
        .into_iter()
        .find_map(|name| {
            bios.attribute(name)
                .and_then(|value| value.str_value().map(str::to_owned))
        });
    Ok(value.and_then(|value| {
        [
            HostPrivilegeLevel::Privileged,
            HostPrivilegeLevel::Restricted,
        ]
        .into_iter()
        .find(|level| host_privilege_value(*level) == value)
    }))
}

/// Stages the host privilege level in BIOS under the spelling the BIOS
/// reports. When it reports neither, or cannot be read in NIC mode, the write
/// uses the firmware's spelling and retries the other when the BMC rejects it.
///
/// The BMC refuses every attribute while the BIOS reports none, as after a
/// BMC restart until the DPU boots, so that is
/// [`PlatformError::NotReady`].
pub(super) async fn set_bios_host_privilege_level<B: Bmc>(
    cx: &OpCx<'_, B>,
    level: HostPrivilegeLevel,
) -> Result<DriverOutcome, PlatformError> {
    let value = host_privilege_value(level);
    let (key, other_key) = if cx.has_quirk(Quirk::BlueFieldSpacedBiosAttributeNames) {
        (HOST_PRIVILEGE_LEVEL_WITH_SPACES, HOST_PRIVILEGE_LEVEL)
    } else {
        (HOST_PRIVILEGE_LEVEL, HOST_PRIVILEGE_LEVEL_WITH_SPACES)
    };
    if !cx.has_quirk(Quirk::BlueFieldNicModeBiosError) {
        let reported = match cx.current_bios_settings().await {
            Ok(settings) if !settings.attributes.is_empty() => settings.attributes,
            Ok(_) | Err(PlatformError::Unsupported) => {
                return Err(PlatformError::NotReady {
                    message: "the BIOS has not reported its attributes since the BMC \
                              restarted; they return once the DPU starts again"
                        .to_string(),
                });
            }
            Err(error) => return Err(error),
        };
        if let Some(reported_key) = [key, other_key]
            .into_iter()
            .find(|name| reported.contains_key(*name))
        {
            return write_bios(cx, &attribute_map([(reported_key, json!(value))])).await;
        }
    }
    match write_bios(cx, &attribute_map([(key, json!(value))])).await {
        Err(PlatformError::Bmc { message, .. }) if message.contains(key) => {
            write_bios(cx, &attribute_map([(other_key, json!(value))])).await
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_500_carrying_nic_mode_counts_as_nic_mode() {
        let bios_error = |status, body: &str| PlatformError::Bmc {
            status,
            message_id: None,
            message: body.to_string(),
        };
        let nic = r#"{"Attributes": {"NicMode": "NicMode"}}"#;
        assert!(bios_error_reports_nic_mode(&bios_error(500, nic)));
        assert!(!bios_error_reports_nic_mode(&bios_error(
            500,
            r#"{"Attributes": {"NicMode": "DpuMode"}}"#
        )));
        assert!(!bios_error_reports_nic_mode(&bios_error(400, nic)));
        assert!(!bios_error_reports_nic_mode(&bios_error(500, "not JSON")));
    }
}
