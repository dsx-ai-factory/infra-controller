/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    BootInterfaceSelector, BootOrder, BootOrderStatus, DriverOutcome, OpCx, PlatformError,
};
use nv_redfish::computer_system::{BootOption, BootOptionUpdate};
use nv_redfish::core::Bmc;
use nv_redfish::schema::computer_system::{BootSource, BootUpdate};

use crate::boot_order::ami::megarac;
use crate::boot_order::standard::StandardBootOrder;
use crate::boot_order::support::{
    RedfishBootOrderExt as _, alias, boot_order, is_first, reference,
};

/// Lenovo GB300 boot behavior: AMI's, and the GB300 firmware also boots
/// through any other enabled network option, so boot order setup enables only
/// the selected interface's HTTP option among them.
pub(crate) struct Gb300BootOrder;

/// The boot options whose enablement boot order setup decides, each with the
/// enablement it wants: the HTTP option enabled, every PXE or HTTP option
/// besides it disabled.
fn wanted_enablement<'a, B: Bmc>(
    options: &'a [BootOption<B>],
    target: &'a BootOption<B>,
) -> impl Iterator<Item = (&'a BootOption<B>, bool)> {
    let target = reference(target);
    options
        .iter()
        .filter(move |option| {
            reference(option) == target
                || matches!(alias(option), Some(BootSource::Pxe | BootSource::UefiHttp))
        })
        .map(move |option| (option, reference(option) == target))
}

#[async_trait]
impl<B: Bmc> BootOrder<B> for Gb300BootOrder {
    fn standard(&self) -> &dyn BootOrder<B> {
        &StandardBootOrder
    }

    async fn status(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<BootOrderStatus, PlatformError> {
        let mac = cx.boot_interface_mac(selector).await?;
        let order = boot_order(cx.system().await?);
        let options = cx.boot_options().await?;
        let target = megarac::http_option(&options, &mac);
        Ok(BootOrderStatus {
            boot_interface_first: target.is_some_and(|target| is_first(&order, reference(target))),
            disk_enabled: None,
            other_network_options_disabled: target.map(|target| {
                wanted_enablement(&options, target)
                    .all(|(option, enabled)| option.enabled() == Some(enabled))
            }),
        })
    }

    async fn set_override(
        &self,
        cx: &OpCx<'_, B>,
        override_setting: &BootUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        megarac::MegaRacBootOrder
            .set_override(cx, override_setting)
            .await
    }

    /// Writes each option's enablement to the settings object it advertises
    /// through `@Redfish.Settings`.
    async fn configure(
        &self,
        cx: &OpCx<'_, B>,
        selector: &BootInterfaceSelector,
    ) -> Result<DriverOutcome, PlatformError> {
        let mac = cx.boot_interface_mac(selector).await?;
        let options = cx.boot_options().await?;
        let target = megarac::http_option(&options, &mac)
            .ok_or_else(|| megarac::missing_http_option(&mac))?;
        let order = boot_order(cx.system().await?);
        let mut outcome = cx.stage_boot_option_first(order, reference(target)).await?;
        for (option, enabled) in wanted_enablement(&options, target) {
            if option.enabled() == Some(enabled) {
                continue;
            }
            let response = option
                .settings()
                .await
                .map_err(|error| cx.map_redfish_error(error))?
                .ok_or(PlatformError::Unsupported)?
                .update(
                    &BootOptionUpdate::builder()
                        .with_boot_option_enabled(enabled)
                        .build(),
                )
                .await
                .map_err(|error| cx.map_redfish_error(error))?;
            outcome = outcome.merge(DriverOutcome::from(response));
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const SYSTEM: &str = "/redfish/v1/Systems/System_0";
    const BOOT_OPTIONS: &str = "/redfish/v1/Systems/System_0/BootOptions";

    /// Adds a boot option that advertises its `SD` settings object, each with
    /// its own ETag, as the GB300 does.
    fn with_option(fixture: Fixture, id: &str, alias: &str, enabled: bool, name: &str) -> Fixture {
        let uri = format!("{BOOT_OPTIONS}/{id}");
        let settings = format!("{uri}/SD");
        let option = |odata_id: &str, resource_id: &str, etag: String| {
            json!({
                "@odata.id": odata_id,
                "@odata.etag": etag,
                "Id": resource_id,
                "Name": format!("Boot{id}"),
                "Alias": alias,
                "BootOptionEnabled": enabled,
                "BootOptionReference": format!("Boot{id}"),
                "DisplayName": name,
            })
        };
        let mut live = option(&uri, id, "\"live\"".to_string());
        live["@Redfish.Settings"] = json!({"SettingsObject": {"@odata.id": settings}});
        fixture
            .document(&uri, live)
            .document(&settings, option(&settings, "SD", format!("\"sd-{id}\"")))
    }

    #[tokio::test]
    async fn option_enablement_goes_to_each_advertised_settings_object() {
        let fixture = Fixture::new("Lenovo", "GB300", "System_0", "HGX_BMC_0")
            .document(
                SYSTEM,
                json!({
                    "@odata.id": SYSTEM,
                    "Id": "System_0",
                    "Name": "System_0",
                    "Boot": {
                        "BootOrder": ["Boot0002", "Boot0005", "Boot0006"],
                        "BootOptions": {"@odata.id": BOOT_OPTIONS},
                    },
                }),
            )
            .document(
                BOOT_OPTIONS,
                json!({
                    "@odata.id": BOOT_OPTIONS,
                    "@odata.type": "#BootOptionCollection.BootOptionCollection",
                    "Name": "Boot Options",
                    "Members": [
                        {"@odata.id": format!("{BOOT_OPTIONS}/0002")},
                        {"@odata.id": format!("{BOOT_OPTIONS}/0005")},
                        {"@odata.id": format!("{BOOT_OPTIONS}/0006")},
                    ],
                }),
            );
        let fixture = with_option(fixture, "0002", "Hdd", true, "ubuntu");
        let fixture = with_option(
            fixture,
            "0005",
            "Pxe",
            true,
            "[SlotFFFF]UEFI: PXE IPv4 Nvidia Network Adapter - D8:94:24:9B:EE:F0",
        );
        let fixture = with_option(
            fixture,
            "0006",
            "UefiHttp",
            false,
            "[SlotFFFF]UEFI: HTTP IPv4 Nvidia Network Adapter - D8:94:24:9B:EE:F0",
        );
        let bmc = fixture.build().await;
        let cx = bmc.cx().await;

        Gb300BootOrder
            .configure(
                &cx,
                &BootInterfaceSelector::Mac("D8:94:24:9B:EE:F0".parse().expect("MAC")),
            )
            .await
            .expect("configure succeeds");

        let option_writes: Vec<_> = bmc
            .writes()
            .iter()
            .filter(|request| path(request).starts_with(BOOT_OPTIONS))
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
            option_writes,
            [
                (
                    format!("{BOOT_OPTIONS}/0005/SD"),
                    "\"sd-0005\"".to_string(),
                    json!({"BootOptionEnabled": false}),
                ),
                (
                    format!("{BOOT_OPTIONS}/0006/SD"),
                    "\"sd-0006\"".to_string(),
                    json!({"BootOptionEnabled": true}),
                ),
            ]
        );
    }
}
