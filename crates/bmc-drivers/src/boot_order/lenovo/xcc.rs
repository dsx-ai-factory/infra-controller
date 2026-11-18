/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::oem::lenovo::boot_manager::{BootOrderKind, LenovoBootManagerCollection};
use nv_redfish::schema::computer_system::BootUpdate;

use crate::boot_order::lenovo::{
    NETWORK, boot_settings, first_group, network_group_first, oem_boot_order, set_next,
};
use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::RedfishBootOrderExt as _;

/// Lenovo XCC 2 boot behavior.
///
/// BIOS setup puts the `Network` group first; boot order setup puts it first
/// again, since status requires it, and puts the adapter first within the OEM
/// `NetworkBootOrder`. XCC can drop `Hard Disk` from its OEM general order,
/// leaving the installed OS unbootable, so configuring restores it and status
/// requires it.
pub(crate) struct XccBootOrder;

const GENERAL_HARD_DISK: &str = "Hard Disk";

/// Whether an OEM network boot entry is the HTTP IPv4 option of the adapter
/// with `mac`, as in `UEFI: SLOT2 (31/0/0) HTTP IPv4  Nvidia Network Adapter - A0:88:C2:08:53:C4`.
/// XCC writes the MAC in uppercase, so `mac` is matched in any case.
fn names_adapter(entry: &str, mac: &str) -> bool {
    let suffix = format!(" - {}", mac.to_ascii_uppercase());
    ["HTTP IPv4  Mellanox", "HTTP IPv4  Nvidia"]
        .iter()
        .any(|prefix| {
            entry
                .find(prefix)
                .is_some_and(|start| entry[start + prefix.len()..].contains(&suffix))
        })
}

async fn settings<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<LenovoBootManagerCollection<B>, PlatformError> {
    boot_settings(cx).await?.ok_or(PlatformError::Unsupported)
}

/// `next` with `Hard Disk` inserted second, or `None` when nothing should be
/// written: it is already listed, or the order is empty, as XCC reports it
/// while the host POSTs, and writing then would drop every other group.
fn with_hard_disk(next: &[String]) -> Option<Vec<String>> {
    if next.is_empty() || next.iter().any(|entry| entry == GENERAL_HARD_DISK) {
        return None;
    }
    let mut next = next.to_vec();
    next.insert(1, GENERAL_HARD_DISK.to_string());
    Some(next)
}

/// Inserts `Hard Disk` second in the OEM general order when it is missing.
async fn restore_hard_disk<B: Bmc>(
    cx: &OpCx<'_, B>,
    settings: &LenovoBootManagerCollection<B>,
) -> Result<DriverOutcome, PlatformError> {
    let general = oem_boot_order(cx, settings, BootOrderKind::General).await?;
    match with_hard_disk(general.next().unwrap_or_default()) {
        Some(next) => set_next(cx, &general, next).await,
        None => Ok(DriverOutcome::complete()),
    }
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for XccBootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        let network_first = first_group(cx).await?.as_deref() == Some(NETWORK);
        let mac = cx.boot_interface_mac(selector).await?;
        let settings = settings(cx).await?;
        let network = oem_boot_order(cx, &settings, BootOrderKind::Network).await?;
        let adapter = network
            .supported()
            .unwrap_or_default()
            .iter()
            .find(|entry| names_adapter(entry, &mac));
        let adapter_first =
            adapter.is_some() && network.next().unwrap_or_default().first() == adapter;
        let general = oem_boot_order(cx, &settings, BootOrderKind::General).await?;
        Ok(BootOrderStatus {
            boot_interface_first: network_first && adapter_first,
            disk_enabled: Some(
                general
                    .next()
                    .unwrap_or_default()
                    .iter()
                    .any(|entry| entry == GENERAL_HARD_DISK),
            ),
            other_network_options_disabled: None,
        })
    }

    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        super::set_override(cx, override_setting).await
    }

    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let mac = cx.boot_interface_mac(selector).await?;
        let settings = settings(cx).await?;
        // Nothing is written until the adapter is listed: while the host POSTs
        // XCC reports empty orders, and writing one would drop its groups.
        let network = oem_boot_order(cx, &settings, BootOrderKind::Network).await?;
        let adapter = network
            .supported()
            .unwrap_or_default()
            .iter()
            .find(|entry| names_adapter(entry, &mac))
            .cloned()
            .ok_or_else(|| PlatformError::MissingBootOption {
                description: format!("XCC NetworkBootOrder HTTP IPv4 option for {mac}"),
            })?;
        let groups = restore_hard_disk(cx, &settings)
            .await?
            .merge(network_group_first(cx).await?);
        let mut next = network.next().unwrap_or_default().to_vec();
        match next.iter().position(|entry| *entry == adapter) {
            Some(0) => return Ok(groups),
            Some(position) => next.swap(0, position),
            None => next.insert(0, adapter),
        }
        Ok(groups.merge(set_next(cx, &network, next).await?))
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn adapter_entries_are_the_http_ipv4_options_naming_the_mac() {
        let mac = "A0:88:C2:08:53:C4";
        for entry in [
            "UEFI:   SLOT2 (31/0/0) HTTP IPv4  Nvidia Network Adapter - A0:88:C2:08:53:C4",
            "UEFI:   SLOT 1 (41/0/0) HTTP IPv4  Nvidia BlueField-3 VPI QSFP112 2P 200G PCIe Gen5 x16 - A0:88:C2:08:53:C4",
            "UEFI:   SLOT1 (4B/0/0) HTTP IPv4  Mellanox Network Adapter - A0:88:C2:08:53:C4",
        ] {
            assert!(names_adapter(entry, mac), "{entry}");
        }
        for entry in [
            "UEFI:   SLOT2 (31/0/0) PXE IPv4  Nvidia Network Adapter - A0:88:C2:08:53:C4",
            "UEFI:   SLOT2 (31/0/0) HTTP IPv4  Nvidia Network Adapter - A0:88:C2:08:53:C5",
            "UEFI:   SLOT1 (4B/0/0) HTTP IPv4  Mellanox Network Adapter - a0:88:c2:08:53:c4",
        ] {
            assert!(!names_adapter(entry, mac), "{entry}");
        }
        assert!(names_adapter(
            "UEFI:   SLOT2 (31/0/0) HTTP IPv4  Nvidia Network Adapter - A0:88:C2:08:53:C4",
            "a0:88:c2:08:53:c4"
        ));
    }

    /// An empty order is what XCC reports while the host POSTs; writing
    /// `Hard Disk` alone over it dropped every other group.
    #[test]
    fn hard_disk_is_restored_only_into_a_reported_order_lacking_it() {
        let order = |entries: &[&str]| entries.iter().map(ToString::to_string).collect::<Vec<_>>();
        value_scenarios!(run = |next: Vec<String>| with_hard_disk(&next);
            "missing" {
                order(&["Network", "CD/DVD Rom"]) => Some(order(&["Network", "Hard Disk", "CD/DVD Rom"])),
                order(&["Network"]) => Some(order(&["Network", "Hard Disk"])),
            }
            "already listed" { order(&["Network", "Hard Disk"]) => None }
            "not reported yet" { order(&[]) => None }
        );
    }
}
