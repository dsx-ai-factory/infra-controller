/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::computer_system::BootOption;
use nv_redfish::core::Bmc;
use nv_redfish::oem::supermicro::SmcFixedBootOrder;
use nv_redfish::oem::supermicro::fixed_boot_order::SmcFixedBootOrderUpdate;
use nv_redfish::schema::computer_system::{BootSource, BootSourceOverrideEnabled, BootUpdate};

use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::{
    RedfishBootOrderExt as _, boot_order, display_name, is_first, persistent_device, reference,
};

/// Supermicro boot behavior.
///
/// The HTTP boot option only appears once BIOS setup has enabled
/// `IPv4HTTPSupport` and the host has rebooted. Models without a usable
/// `Boot.BootOrder` expose the OEM `FixedBootOrder` resource instead, which
/// orders device classes and the UEFI network adapters separately. An
/// override without a mode boots UEFI.
pub(crate) struct SmcBootOrder;

const MELLANOX_HTTP_IPV4: &str = "UEFI HTTP IPv4 Mellanox Network Adapter";
const NVIDIA_HTTP_IPV4: &str = "UEFI HTTP IPv4 Nvidia Network Adapter";
const NETWORK: &str = "UEFI Network";
const HARD_DISK: &str = "UEFI Hard Disk";

fn is_http_adapter(name: &str) -> bool {
    name.contains(MELLANOX_HTTP_IPV4) || name.contains(NVIDIA_HTTP_IPV4)
}

/// Whether a boot option or `UEFINetwork` entry is the HTTP IPv4 option of
/// the adapter with `mac`, as in
/// `UEFI HTTP IPv4 Mellanox Network Adapter - A0:88:C2:EA:84:D0(MAC:A088C2EA84D0)`.
fn names_adapter(name: &str, mac: &str) -> bool {
    is_http_adapter(name)
        && name
            .to_ascii_uppercase()
            .contains(&mac.to_ascii_uppercase())
}

/// Whether the BMC rejected a `Boot.BootOrder` write because this model has
/// no such property.
fn boot_order_unknown(error: &PlatformError) -> bool {
    matches!(
        error,
        PlatformError::Bmc { status: 400, message_id, message }
            if (message.contains("PropertyUnknown")
                || message_id.as_deref().is_some_and(|id| id.contains("PropertyUnknown")))
                && message.contains("BootOrder")
    )
}

async fn fixed_boot_order<B: Bmc>(cx: &OpCx<'_, B>) -> Result<SmcFixedBootOrder<B>, PlatformError> {
    cx.system()
        .await?
        .oem_supermicro()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .fixed_boot_order()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)
}

/// The class entry the BMC reports for `class`, which carries device
/// specifics, or the bare class name.
fn class_entry(fixed_boot_order: &[String], class: &str) -> String {
    fixed_boot_order
        .iter()
        .find(|entry| entry.starts_with(class))
        .cloned()
        .unwrap_or_else(|| class.to_string())
}

/// Writes `first` and `second` as the only enabled device classes, every
/// other class disabled, with `uefi_network` as the adapter order.
async fn write_fixed_boot_order<B: Bmc>(
    cx: &OpCx<'_, B>,
    resource: &SmcFixedBootOrder<B>,
    first: String,
    second: String,
    uefi_network: Vec<String>,
) -> Result<DriverOutcome, PlatformError> {
    let slots = resource.fixed_boot_order().unwrap_or_default().len().max(2);
    let mut order = vec!["Disabled".to_string(); slots];
    order[0] = first;
    order[1] = second;
    resource
        .update(
            &SmcFixedBootOrderUpdate::builder()
                .with_fixed_boot_order(order)
                .with_uefi_network(uefi_network)
                .build(),
        )
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

/// Moves the adapter's HTTP option first in the standard boot order on the
/// live system; `None` when the system reports no boot order to change.
async fn standard_adapter_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    mac: &str,
) -> Result<Option<DriverOutcome>, PlatformError> {
    let order = boot_order(cx.system().await?);
    if order.is_empty() {
        return Ok(None);
    }
    let options = cx.boot_options().await?;
    let target = options
        .iter()
        .find(|option| names_adapter(display_name(option), mac))
        .map(reference)
        .ok_or_else(|| PlatformError::MissingBootOption {
            description: format!("HTTP IPv4 Mellanox or Nvidia adapter boot option for {mac}"),
        })?;
    cx.put_system_boot_option_first(order, target)
        .await
        .map(Some)
}

/// Puts the network class first and disks second in the fixed boot order,
/// and the adapter's HTTP option first among the UEFI network adapters.
async fn fixed_adapter_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    mac: &str,
) -> Result<DriverOutcome, PlatformError> {
    let resource = fixed_boot_order(cx).await?;
    let classes = resource.fixed_boot_order().unwrap_or_default();
    let mut uefi_network = resource.uefi_network().unwrap_or_default().to_vec();
    let position = uefi_network
        .iter()
        .position(|entry| names_adapter(entry, mac))
        .ok_or_else(|| PlatformError::MissingBootOption {
            description: format!("UEFINetwork HTTP IPv4 Mellanox or Nvidia adapter for {mac}"),
        })?;
    uefi_network.swap(0, position);
    let network = class_entry(classes, NETWORK);
    let hard_disk = class_entry(classes, HARD_DISK);
    write_fixed_boot_order(cx, &resource, network, hard_disk, uefi_network).await
}

/// Boots `device` from now on through the fixed boot order: its class first,
/// the other of network and disk second, and for network boot the first
/// HTTP IPv4 adapter first among the UEFI network adapters.
async fn fixed_device_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    device: BootSource,
) -> Result<DriverOutcome, PlatformError> {
    let resource = fixed_boot_order(cx).await?;
    let network = class_entry(resource.fixed_boot_order().unwrap_or_default(), NETWORK);
    let mut uefi_network = resource.uefi_network().unwrap_or_default().to_vec();
    if device == BootSource::Hdd {
        return write_fixed_boot_order(cx, &resource, HARD_DISK.to_string(), network, uefi_network)
            .await;
    }
    let position = uefi_network
        .iter()
        .position(|entry| is_http_adapter(entry))
        .ok_or_else(|| PlatformError::MissingBootOption {
            description: "UEFINetwork HTTP IPv4 Mellanox or Nvidia adapter".to_string(),
        })?;
    uefi_network.swap(0, position);
    write_fixed_boot_order(cx, &resource, network, HARD_DISK.to_string(), uefi_network).await
}

/// Whether the standard boot order starts with the adapter's HTTP option.
fn standard_adapter_is_first<B: Bmc>(
    order: &[String],
    options: &[BootOption<B>],
    mac: &str,
) -> bool {
    options
        .iter()
        .find(|option| names_adapter(display_name(option), mac))
        .is_some_and(|target| is_first(order, reference(target)))
}

/// Whether the fixed boot order boots the adapter's HTTP option first: the
/// first class is the network class with the adapter first among the UEFI
/// network adapters, or a class entry naming the adapter itself.
fn fixed_adapter_is_first(classes: &[String], uefi_network: &[String], mac: &str) -> bool {
    let expected = uefi_network.iter().find(|entry| names_adapter(entry, mac));
    let actual = classes.first().map(|entry| {
        let network_class = entry
            .strip_prefix(NETWORK)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(':'));
        if network_class {
            uefi_network.first().unwrap_or(entry).clone()
        } else if let Some((_, option)) = entry.split_once(':') {
            option.trim().to_string()
        } else {
            entry.clone()
        }
    });
    expected.is_some() && expected == actual.as_ref()
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for SmcBootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        let mac = cx.boot_interface_mac(selector).await?;
        let first = match cx.boot_options().await {
            Ok(options) => {
                let order = boot_order(cx.system().await?);
                if order.is_empty() {
                    None
                } else {
                    Some(standard_adapter_is_first(&order, &options, &mac))
                }
            }
            Err(error) if boot_order_unknown(&error) => None,
            Err(error) => return Err(error),
        };
        let boot_interface_first = match first {
            Some(first) => first,
            None => {
                let resource = fixed_boot_order(cx).await?;
                fixed_adapter_is_first(
                    resource.fixed_boot_order().unwrap_or_default(),
                    resource.uefi_network().unwrap_or_default(),
                    &mac,
                )
            }
        };
        Ok(BootOrderStatus {
            boot_interface_first,
            disk_enabled: None,
            other_network_options_disabled: None,
        })
    }

    /// A persistent device goes first in the fixed boot order, or becomes a
    /// continuous override on models without one.
    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        let Some(device) = persistent_device(override_setting) else {
            return cx.set_system_boot_override(override_setting, true).await;
        };
        match fixed_device_first(cx, device).await {
            Err(PlatformError::Unsupported | PlatformError::Bmc { status: 404, .. }) => {
                let continuous = BootUpdate::builder()
                    .with_boot_source_override_target(device)
                    .with_boot_source_override_enabled(BootSourceOverrideEnabled::Continuous)
                    .build();
                cx.set_system_boot_override(&continuous, true).await
            }
            result => result,
        }
    }

    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let mac = cx.boot_interface_mac(selector).await?;
        match standard_adapter_first(cx, &mac).await {
            Ok(Some(outcome)) => Ok(outcome),
            Ok(None) => fixed_adapter_first(cx, &mac).await,
            Err(error) if boot_order_unknown(&error) => fixed_adapter_first(cx, &mac).await,
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: &str = "B8:E9:24:17:6D:72";
    const ADAPTER: &str =
        "UEFI HTTP IPv4 Nvidia Network Adapter - B8:E9:24:17:6D:72 - B8E924176D72";

    fn entries(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|entry| (*entry).to_string()).collect()
    }

    #[test]
    fn fixed_boot_order_boots_the_adapter_first_only_through_its_http_option() {
        let cases = [
            (
                "network class with the adapter first",
                &["UEFI Network:(B2/D0/F0) NVIDIA", "UEFI Hard Disk"][..],
                &[ADAPTER, "UEFI PXE IPv4 Nvidia - B8:E9:24:17:6D:72"][..],
                true,
            ),
            (
                "adapter second among the adapters",
                &["UEFI Network"],
                &["UEFI PXE IPv4 Nvidia - B8:E9:24:17:6D:72", ADAPTER],
                false,
            ),
            (
                "disks first",
                &["UEFI Hard Disk", "UEFI Network"],
                &[ADAPTER],
                false,
            ),
            (
                "a PXE entry for the MAC only",
                &["UEFI Network"],
                &["UEFI PXE IPv4 Nvidia - B8:E9:24:17:6D:72"],
                false,
            ),
        ];
        for (name, classes, uefi_network, expected) in cases {
            assert_eq!(
                fixed_adapter_is_first(&entries(classes), &entries(uefi_network), MAC),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn only_property_unknown_boot_order_rejections_fall_back_to_the_fixed_boot_order() {
        let rejected = PlatformError::Bmc {
            status: 400,
            message_id: Some("Base.1.4.PropertyUnknown".to_string()),
            message: "The property BootOrder is not in the list of valid properties".to_string(),
        };
        assert!(boot_order_unknown(&rejected));
        let other = PlatformError::Bmc {
            status: 400,
            message_id: Some("Base.1.4.PropertyValueNotInList".to_string()),
            message: "BootOrder".to_string(),
        };
        assert!(!boot_order_unknown(&other));
    }
}
