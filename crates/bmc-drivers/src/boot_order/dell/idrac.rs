/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::computer_system::{BootOption, ComputerSystemUpdate};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::schema::computer_system::{BootSource, BootSourceOverrideEnabled, BootUpdate};
use serde_json::{Value, json};

use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::{RedfishBootOrderExt as _, boot_order, display_name, reference};
use crate::dell::{self, ManagerApplyTime};
use crate::resources::{RedfishResourcesExt as _, attribute_map, bios_update};

/// Dell iDRAC boot behavior.
///
/// iDRAC rejects `BootSourceOverrideTarget` writes: a PXE or disk override is
/// the iDRAC `ServerBoot` first boot device, and a UEFI HTTP boot URI is pinned
/// through the `HttpDev1*` BIOS attributes as a configuration job. BIOS setup
/// points HTTP Device 1 at the boot interface's NIC, so boot order setup puts
/// that device first.
pub(crate) struct IdracBootOrder;

/// The boot option HTTP Device 1 appears as once BIOS setup points it at the
/// selected interface's NIC.
async fn http_device_name<B: Bmc>(
    cx: &OpCx<'_, B>,
    selector: &BootInterfaceSelector,
) -> Result<String, PlatformError> {
    let description = dell::nic(cx, selector)
        .await?
        .get("DeviceDescription")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: format!("DellNIC for {selector:?} reports no DeviceDescription"),
        })?;
    Ok(format!("HTTP Device 1: {description}"))
}

/// `boot_option_id` as the whole boot order, staged for the next reset:
/// iDRAC leaves a boot order change without an apply time unapplied.
fn boot_order_update(boot_option_id: String) -> ComputerSystemUpdate {
    ComputerSystemUpdate::builder()
        .with_boot(
            BootUpdate::builder()
                .with_boot_order(vec![boot_option_id])
                .build(),
        )
        .build()
        .with_settings_apply_time(dell::on_reset())
}

/// iDRAC may append ` - <detail>` to the boot option's name.
fn name_matches(expected: &str, actual: &str) -> bool {
    actual == expected
        || actual
            .strip_prefix(expected)
            .is_some_and(|suffix| suffix.starts_with(" - "))
}

/// The boot options `order` lists, in order, skipping entries naming none.
fn ordered<'a, B: Bmc>(order: &[String], options: &'a [BootOption<B>]) -> Vec<&'a BootOption<B>> {
    order
        .iter()
        .filter_map(|entry| options.iter().find(|option| reference(option) == entry))
        .collect()
}

/// Some iDRACs report `HttpDev1Uri` as read-only (MessageId `IDRAC.*.SYS410`)
/// for reasons that have not been isolated; callers then fall back to DHCP.
fn read_only_attribute_is_unsupported(error: PlatformError) -> PlatformError {
    if dell::is_read_only_attribute(&error) {
        PlatformError::Unsupported
    } else {
        error
    }
}

async fn pin_http_boot_uri<B: Bmc>(
    cx: &OpCx<'_, B>,
    uri: &str,
) -> Result<DriverOutcome, PlatformError> {
    let body = bios_update(&attribute_map([
        ("HttpDev1Uri", json!(uri)),
        ("HttpDev1EnDis", json!("Enabled")),
        ("HttpDev1DhcpEnDis", json!("Disabled")),
        ("HttpDev1Protocol", json!("IPv4")),
    ]))?
    .with_settings_apply_time(dell::on_reset());
    let response = cx
        .update_bios_settings(&body)
        .await
        .map_err(read_only_attribute_is_unsupported)?;
    dell::job_outcome(cx, response).await
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for IdracBootOrder
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        let expected = http_device_name(cx, selector).await?;
        let order = boot_order(cx.system().await?);
        let options = cx.boot_options().await?;
        Ok(BootOrderStatus {
            boot_interface_first: ordered(&order, &options)
                .first()
                .is_some_and(|option| name_matches(&expected, display_name(option))),
            disk_enabled: None,
            other_network_options_disabled: None,
        })
    }

    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        if let Some(uri) = override_setting.http_boot_uri.as_deref() {
            return pin_http_boot_uri(cx, uri).await;
        }
        let device = match override_setting.boot_source_override_target {
            Some(BootSource::Pxe) => "PXE",
            Some(BootSource::Hdd) => "HDD",
            _ => return Err(PlatformError::Unsupported),
        };
        let boot_once = match override_setting.boot_source_override_enabled {
            Some(BootSourceOverrideEnabled::Once) => "Enabled",
            Some(BootSourceOverrideEnabled::Continuous) => "Disabled",
            _ => return Err(PlatformError::Unsupported),
        };
        let attributes = attribute_map([
            ("ServerBoot.1.FirstBootDevice", json!(device)),
            ("ServerBoot.1.BootOnce", json!(boot_once)),
        ]);
        dell::patch_manager_attributes(cx, &attributes, Some(ManagerApplyTime::OnReset)).await
    }

    /// Stages HTTP Device 1 as the whole boot order in a configuration job;
    /// iDRAC creates no job when it is already first.
    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let expected = http_device_name(cx, selector).await?;
        let order = boot_order(cx.system().await?);
        let options = cx.boot_options().await?;
        let ordered = ordered(&order, &options);
        let position = ordered
            .iter()
            .position(|option| name_matches(&expected, display_name(option)))
            .ok_or(PlatformError::MissingBootOption {
                description: expected,
            })?;
        if position == 0 {
            return Ok(DriverOutcome::complete());
        }
        dell::clear_job_queue(cx).await?;
        let system = cx.system().await?;
        let settings = system
            .settings()
            .await
            .map_err(|error| cx.map_redfish_error(error))?;
        let update = boot_order_update(ordered[position].raw().id.clone());
        let response = settings
            .as_ref()
            .unwrap_or(system)
            .update(&update)
            .await
            .map_err(|error| cx.map_redfish_error(error))?;
        dell::job_outcome(cx, response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_order_change_applies_on_reset() {
        assert_eq!(
            serde_json::to_value(boot_order_update("Boot0000".to_string())).expect("serializes"),
            json!({
                "@Redfish.SettingsApplyTime": {"ApplyTime": "OnReset"},
                "Boot": {"BootOrder": ["Boot0000"]},
            })
        );
    }

    #[test]
    fn read_only_http_uri_rejection_is_unsupported() {
        let read_only = PlatformError::Bmc {
            status: 400,
            message_id: Some("IDRAC.2.9.SYS410".to_string()),
            message: "attribute is read-only".to_string(),
        };
        assert_eq!(
            read_only_attribute_is_unsupported(read_only),
            PlatformError::Unsupported
        );
        let other = PlatformError::Bmc {
            status: 400,
            message_id: Some("Base.1.0.GeneralError".to_string()),
            message: "bad request".to_string(),
        };
        assert_eq!(read_only_attribute_is_unsupported(other.clone()), other);
    }
}
