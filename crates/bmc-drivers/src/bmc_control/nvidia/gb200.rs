/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BmcControl, DriverOutcome, ManagerSettings, ManagerSettingsDiff, ManagerSettingsStatus, OpCx,
    PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::ethernet_interface::{EthernetInterface, EthernetInterfaceUpdate};
use nv_redfish::schema::ethernet_interface::EthernetInterface as EthernetInterfaceSchema;
use nv_redfish::schema::ip_addresses::{Ipv4address, Ipv4addressUpdate};
use serde_json::{Value, json};

use crate::bmc_control::standard::StandardBmcControl;

/// NVIDIA GB200: the BMC reaches the host through `hostusb0`, which needs
/// the static IPv4 address the host side expects.
pub(crate) struct Gb200BmcControl;

const HOST_INTERFACE_ID: &str = "hostusb0";
const ADDRESS: &str = "10.0.1.1";
const SUBNET_MASK: &str = "255.255.255.0";
const GATEWAY: &str = "0.0.0.0";

async fn host_interface<B: Bmc>(cx: &OpCx<'_, B>) -> Result<EthernetInterface<B>, PlatformError> {
    cx.manager()
        .await?
        .ethernet_interfaces()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .members()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .into_iter()
        .find(|interface| interface.raw().id == HOST_INTERFACE_ID)
        .ok_or(PlatformError::Unsupported)
}

/// An unreported list is an error rather than an empty one, so nothing is
/// written over addresses the BMC did not show.
fn static_addresses(interface: &EthernetInterfaceSchema) -> Result<&[Ipv4address], PlatformError> {
    interface
        .ipv4static_addresses
        .as_ref()
        .and_then(Option::as_deref)
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: format!("{HOST_INTERFACE_ID} did not report IPv4StaticAddresses"),
        })
}

fn reported(value: &Option<Option<String>>) -> Option<&str> {
    value.as_ref().and_then(Option::as_deref)
}

/// Exactly the expected address; a missing gateway counts as `0.0.0.0`.
fn is_configured(addresses: &[Ipv4address]) -> bool {
    matches!(addresses, [address]
        if reported(&address.address) == Some(ADDRESS)
            && reported(&address.subnet_mask) == Some(SUBNET_MASK)
            && matches!(reported(&address.gateway), None | Some(GATEWAY)))
}

fn address_json(address: &Ipv4address) -> Value {
    json!({
        "Address": reported(&address.address),
        "SubnetMask": reported(&address.subnet_mask),
        "Gateway": reported(&address.gateway),
    })
}

#[async_trait]
impl<B: Bmc> BmcControl<B> for Gb200BmcControl
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn BmcControl<B> {
        &StandardBmcControl
    }

    /// Adds the address only when the list is empty; any other address is
    /// left for an operator. The update carries the interface's ETag, so a
    /// concurrent change fails instead of being overwritten.
    async fn apply_settings(
        &self,
        cx: &OpCx<'_, B>,
        profile: &ManagerSettings,
    ) -> Result<DriverOutcome, PlatformError> {
        if !profile.attributes.is_empty() {
            return Err(PlatformError::Unsupported);
        }
        let interface = host_interface(cx).await?;
        let raw = interface.raw();
        let addresses = static_addresses(&raw)?;
        if is_configured(addresses) {
            return Ok(DriverOutcome::complete());
        }
        if !addresses.is_empty() {
            return Err(PlatformError::ManualInterventionRequired {
                code: "gb200-host-interface-ipv4-conflict".to_string(),
            });
        }
        let update = EthernetInterfaceUpdate::builder()
            .with_ipv4static_addresses(vec![
                Ipv4addressUpdate::builder()
                    .with_address(ADDRESS.to_string())
                    .with_subnet_mask(SUBNET_MASK.to_string())
                    .with_gateway(GATEWAY.to_string())
                    .build(),
            ])
            .build();
        interface
            .update(&update)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    async fn settings_status(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<ManagerSettingsStatus, PlatformError> {
        let interface = host_interface(cx).await?.raw();
        let addresses = static_addresses(&interface)?;
        let differences: Vec<ManagerSettingsDiff> = (!is_configured(addresses))
            .then(|| ManagerSettingsDiff {
                key: format!("{HOST_INTERFACE_ID}.IPv4StaticAddresses"),
                expected: json!([{"Address": ADDRESS, "SubnetMask": SUBNET_MASK, "Gateway": GATEWAY}]),
                actual: Some(Value::Array(addresses.iter().map(address_json).collect())),
            })
            .into_iter()
            .collect();
        Ok(ManagerSettingsStatus {
            is_applied: differences.is_empty(),
            differences,
        })
    }
}
