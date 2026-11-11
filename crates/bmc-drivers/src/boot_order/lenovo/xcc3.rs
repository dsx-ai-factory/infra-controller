/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::schema::computer_system::BootUpdate;
use serde_json::Value;

use crate::bios::lenovo::ensure_bios_settings_unlocked;
use crate::boot_order::lenovo::{NETWORK, first_group};
use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::RedfishBootOrderExt as _;
use crate::resources::{RedfishResourcesExt as _, bios_attributes};

/// Lenovo XCC 3 boot behavior.
///
/// XCC 3 has no OEM boot settings: BIOS setup puts the `Network` group first,
/// and boot order setup puts the adapter first among the
/// `BootOrder_NetworkPriority_*` BIOS attributes.
pub(crate) struct Xcc3BootOrder;

const NETWORK_PRIORITY: &str = "BootOrder_NetworkPriority_";
/// The network priority attributes XCC 3 exposes.
const NETWORK_PRIORITY_SLOTS: RangeInclusive<u32> = 1..=10;

/// Whether a network priority value is the HTTP IPv4 option of the adapter
/// with `mac`, as in `Slot5Port1HTTPv4NvidiaNetworkAdapter_B8_E9_24_18_42_52`.
fn names_adapter(value: &str, mac: &str) -> bool {
    value
        .to_ascii_uppercase()
        .contains(&mac.replace(':', "_").to_ascii_uppercase())
        && value.contains("HTTPv4")
}

/// The network priority attributes by slot.
async fn network_priorities<B: Bmc>(
    cx: &OpCx<'_, B>,
) -> Result<BTreeMap<u32, String>, PlatformError> {
    let attributes = bios_attributes(&cx.bios().await?.raw());
    Ok(NETWORK_PRIORITY_SLOTS
        .filter_map(|slot| {
            attributes
                .get(&format!("{NETWORK_PRIORITY}{slot}"))
                .and_then(Value::as_str)
                .map(|value| (slot, value.to_string()))
        })
        .collect())
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for Xcc3BootOrder {
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
        let priorities = network_priorities(cx).await?;
        let adapter = priorities.values().find(|value| names_adapter(value, &mac));
        Ok(BootOrderStatus {
            boot_interface_first: network_first
                && adapter.is_some()
                && adapter == priorities.get(&1),
            disk_enabled: None,
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

    /// Swaps the adapter's network priority into the first slot.
    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let mac = cx.boot_interface_mac(selector).await?;
        let priorities = network_priorities(cx).await?;
        let (slot, adapter) = priorities
            .iter()
            .find(|(_, value)| names_adapter(value, &mac))
            .ok_or_else(|| PlatformError::MissingBootOption {
                description: format!("{NETWORK_PRIORITY}* HTTPv4 option for {mac}"),
            })?;
        if *slot == 1 {
            return Ok(DriverOutcome::complete());
        }
        let mut swapped = BTreeMap::from([(
            format!("{NETWORK_PRIORITY}1"),
            Value::from(adapter.as_str()),
        )]);
        if let Some(first) = priorities.get(&1) {
            swapped.insert(
                format!("{NETWORK_PRIORITY}{slot}"),
                Value::from(first.as_str()),
            );
        }
        ensure_bios_settings_unlocked(cx).await?;
        cx.stage_bios_attributes(&swapped).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities_name_the_adapter_by_its_http_ipv4_option() {
        let mac = "b8:e9:24:18:42:52";
        assert!(names_adapter(
            "Slot5Port1HTTPv4NvidiaNetworkAdapter_B8_E9_24_18_42_52",
            mac
        ));
        assert!(!names_adapter(
            "Slot5Port1PXEv4NvidiaNetworkAdapter_B8_E9_24_18_42_52",
            mac
        ));
    }
}
