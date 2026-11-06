/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::computer_system::ComputerSystemUpdate;
use nv_redfish::core::Bmc;
use nv_redfish::oem::hpe::server_boot_settings::HpeServerBootSettingsUpdate;
use nv_redfish::schema::computer_system::{BootSource, BootUpdate};
use serde_json::json;

use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::{RedfishBootOrderExt as _, boot_order, display_name, reference};
use crate::resources::{RedfishResourcesExt as _, attribute_map};

/// HPE iLO boot behavior.
///
/// iLO rejects `BootSourceOverride` writes. A UEFI HTTP URI is pinned through
/// BIOS attributes; any other override moves the target's category to the
/// front of the persistent boot order.
pub(crate) struct IloBootOrder;

#[async_trait]
impl<B: Bmc> BootOrder<B> for IloBootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        if let Some(uri) = override_setting.http_boot_uri.as_deref() {
            return cx
                .stage_bios_attributes(&attribute_map([
                    ("UrlBootFile", json!(uri)),
                    ("PreBootNetwork", json!("IPv4")),
                ]))
                .await;
        }
        let category = match override_setting.boot_source_override_target {
            Some(BootSource::Pxe | BootSource::UefiHttp) => "nic.",
            Some(BootSource::Hdd) => "hd.",
            _ => return Err(PlatformError::Unsupported),
        };
        set_persistent_boot_first(cx, category).await
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        let http_option = http_option(cx, selector).await?;
        let order = boot_order(cx.system().await?);
        Ok(BootOrderStatus {
            boot_interface_first: order.first() == Some(&http_option),
            disk_enabled: None,
            other_network_options_disabled: None,
        })
    }

    /// Swaps the HTTP option into the first slot of the live boot order. iLO
    /// refuses boot-order changes while the host is in POST; that refusal is
    /// not an error, and the next status read shows whether the order holds.
    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let http_option = http_option(cx, selector).await?;
        let mut order = boot_order(cx.system().await?);
        let position = order
            .iter()
            .position(|entry| *entry == http_option)
            .ok_or_else(|| PlatformError::MissingBootOption {
                description: format!("BootOrder entry {http_option}"),
            })?;
        order.swap(0, position);
        let update = ComputerSystemUpdate::builder()
            .with_boot(BootUpdate::builder().with_boot_order(order).build())
            .build();
        match cx
            .system()
            .await?
            .update(&update)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
        {
            Err(PlatformError::Bmc {
                message_id,
                message,
                ..
            }) if message_id
                .as_deref()
                .is_some_and(|id| id.ends_with(UNABLE_TO_MODIFY_DURING_POST))
                || message.contains(UNABLE_TO_MODIFY_DURING_POST) =>
            {
                Ok(DriverOutcome::complete())
            }
            result => result,
        }
    }
}

const UNABLE_TO_MODIFY_DURING_POST: &str = "UnableToModifyDuringSystemPOST";

/// The reference of the selected interface's HTTP IPv4 boot option: the
/// first whose display name holds `HTTP`, `IPV4` and the MAC.
async fn http_option<B: Bmc>(
    cx: &OpCx<'_, B>,
    selector: &BootInterfaceSelector,
) -> Result<String, PlatformError> {
    let mac = cx.boot_interface_mac(selector).await?.to_uppercase();
    cx.boot_options()
        .await?
        .iter()
        .find(|option| {
            let name = display_name(option).to_uppercase();
            name.contains("HTTP") && name.contains("IPV4") && name.contains(&mac)
        })
        .map(|option| reference(option).to_string())
        .ok_or(PlatformError::MissingBootOption {
            description: format!("HTTP IPv4 {mac}"),
        })
}

/// Moves every persistent boot entry naming `category` to the front.
async fn set_persistent_boot_first<B: Bmc>(
    cx: &OpCx<'_, B>,
    category: &str,
) -> Result<DriverOutcome, PlatformError> {
    let boot = cx
        .bios()
        .await?
        .oem_hpe()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .boot_settings()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?;
    let order =
        boot.persistent_boot_config_order()
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: "HPE boot settings report no PersistentBootConfigOrder".to_string(),
            })?;
    let order = category_first(order, category);
    boot.settings()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or(PlatformError::Unsupported)?
        .update(
            &HpeServerBootSettingsUpdate::builder()
                .with_persistent_boot_config_order(order)
                .build(),
        )
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

/// Each entry naming `category` is inserted at the front as it is met, so
/// matching entries end up first in reverse order.
fn category_first(order: &[String], category: &str) -> Vec<String> {
    let mut reordered = Vec::with_capacity(order.len());
    for entry in order {
        if entry.to_ascii_lowercase().contains(category) {
            reordered.insert(0, entry.clone());
        } else {
            reordered.push(entry.clone());
        }
    }
    reordered
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    /// iLO 5 v2.72 answers `If-Match: *` with 412, so the boot order goes
    /// with the system's own ETag.
    #[tokio::test]
    async fn the_boot_order_is_written_with_the_system_etag() {
        const SYSTEM: &str = "/redfish/v1/Systems/1";
        const BOOT_OPTIONS: &str = "/redfish/v1/Systems/1/BootOptions";
        let option = |id: &str, name: &str| {
            json!({"@odata.id": format!("{BOOT_OPTIONS}/{id}"), "Id": id, "Name": id,
                   "BootOptionReference": format!("Boot{id}"), "DisplayName": name})
        };
        let bmc = Fixture::new("HPE", "", "1", "1")
            .document(
                SYSTEM,
                json!({"@odata.id": SYSTEM, "@odata.etag": "W/\"B01D9781\"", "Id": "1",
                       "Name": "Computer System",
                       "Boot": {"BootOrder": ["Boot0001", "Boot0002"],
                                "BootOptions": {"@odata.id": BOOT_OPTIONS}}}),
            )
            .document(
                BOOT_OPTIONS,
                json!({"@odata.id": BOOT_OPTIONS, "Name": "Boot Options",
                       "@odata.type": "#BootOptionCollection.BootOptionCollection",
                       "Members": [{"@odata.id": format!("{BOOT_OPTIONS}/0001")},
                                   {"@odata.id": format!("{BOOT_OPTIONS}/0002")}]}),
            )
            .document(
                &format!("{BOOT_OPTIONS}/0001"),
                option("0001", "Embedded RAID"),
            )
            .document(
                &format!("{BOOT_OPTIONS}/0002"),
                option("0002", "HTTP IPv4 Network Adapter - B8:3F:D2:D7:F7:4A"),
            )
            .build()
            .await;
        let cx = bmc.cx().await;
        IloBootOrder
            .configure(
                &cx,
                &BootInterfaceSelector::Mac("B8:3F:D2:D7:F7:4A".parse().expect("MAC")),
            )
            .await
            .expect("configure succeeds");
        let writes: Vec<_> = bmc
            .writes()
            .iter()
            .map(|request| {
                (
                    path(request).to_string(),
                    request.headers["if-match"]
                        .to_str()
                        .expect("ASCII")
                        .to_string(),
                    body(request),
                )
            })
            .collect();
        assert_eq!(
            writes,
            [(
                SYSTEM.to_string(),
                "W/\"B01D9781\"".to_string(),
                json!({"Boot": {"BootOrder": ["Boot0002", "Boot0001"]}}),
            )]
        );
    }

    #[test]
    fn category_entries_move_to_the_front_in_reverse_order() {
        let order = [
            "HD.EmbRAID.1-1",
            "NIC.Slot.1-1",
            "Generic.USB.1-1",
            "NIC.Slot.2-1",
        ]
        .map(str::to_string);
        assert_eq!(
            category_first(&order, "nic."),
            [
                "NIC.Slot.2-1",
                "NIC.Slot.1-1",
                "HD.EmbRAID.1-1",
                "Generic.USB.1-1"
            ]
        );
    }
}
