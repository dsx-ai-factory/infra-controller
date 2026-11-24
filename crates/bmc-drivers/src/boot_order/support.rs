/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Boot-order reads and writes shared by the boot-order drivers.
//!
//! Each driver finds the selected interface's boot option and writes the boot
//! order the way its platform's hardware-verified integration does; these are
//! the pieces they have in common.

use bmc_platform::{BootInterfaceSelector, DriverOutcome, OpCx, PlatformError};
use nv_redfish::computer_system::{
    BootOption, BootOptionReference, ComputerSystem, ComputerSystemUpdate,
};
use nv_redfish::core::{Bmc, ModificationResponse, ODataETag, ODataId};
use nv_redfish::schema::computer_system::{
    BootSource, BootSourceOverrideEnabled, BootSourceOverrideMode, BootUpdate,
};
use serde_json::Value;

/// The option's display name; empty when the BMC omits it.
pub(crate) fn display_name<B: Bmc>(option: &BootOption<B>) -> &str {
    option.display_name().map_or("", |name| *name.inner())
}

/// The option's `BootOptionReference`, or its id when the BMC omits it.
pub(crate) fn reference<B: Bmc>(option: &BootOption<B>) -> &str {
    option.boot_reference().inner()
}

/// The option's `Alias`.
pub(crate) fn alias<B: Bmc>(option: &BootOption<B>) -> Option<BootSource> {
    option.raw().alias.flatten()
}

/// Whether the option is the one `entry` of a `BootOrder` names, by
/// reference or resource id.
fn names_option<B: Bmc>(entry: &str, option: &BootOption<B>) -> bool {
    let entry = entry_reference(entry);
    reference(option) == entry || option.raw().id == entry
}

/// The selected system's `Boot.BootOrder` as reported; empty when absent.
pub(crate) fn boot_order<B: Bmc>(system: &ComputerSystem<B>) -> Vec<String> {
    system
        .boot_order()
        .unwrap_or_default()
        .into_iter()
        .map(|reference| reference.inner().to_string())
        .collect()
}

/// The boot option reference a `BootOrder` entry names; some firmware reports
/// entries as `Boot0001: <display name>`.
fn entry_reference(entry: &str) -> &str {
    entry
        .split_once(": ")
        .map_or(entry, |(reference, _)| reference)
}

/// Moves the entry naming `reference` to the front of `order`, keeping the
/// entry as the BMC reports it; `false` when no entry names it.
pub(crate) fn promote(order: &mut Vec<String>, reference: &str) -> bool {
    let Some(position) = order
        .iter()
        .position(|entry| entry_reference(entry) == reference)
    else {
        return false;
    };
    let entry = order.remove(position);
    order.retain(|entry| entry_reference(entry) != reference);
    order.insert(0, entry);
    true
}

/// `order` with the entry naming `reference` first, inserting `reference`
/// when no entry names it.
fn with_first(mut order: Vec<String>, reference: &str) -> Vec<String> {
    if !promote(&mut order, reference) {
        order.insert(0, reference.to_string());
    }
    order
}

/// Whether `order` starts with the entry naming `reference`.
pub(crate) fn is_first(order: &[String], reference: &str) -> bool {
    order
        .first()
        .is_some_and(|entry| entry_reference(entry) == reference)
}

/// The boot option `entry` of a `BootOrder` names. The BMC lists boot
/// options again as the host starts, so for a while after a restart the
/// order can name one it does not list yet; that is not ready.
pub(crate) fn listed_option<'a, B: Bmc>(
    entry: &str,
    options: &'a [BootOption<B>],
) -> Result<&'a BootOption<B>, PlatformError> {
    options
        .iter()
        .find(|option| names_option(entry, option))
        .ok_or_else(|| PlatformError::NotReady {
            message: format!("BootOrder lists {entry}, which is not a boot option yet"),
        })
}

/// The boot options `order` lists, in order.
pub(crate) fn listed_options<'a, B: Bmc>(
    order: &[String],
    options: &'a [BootOption<B>],
) -> Result<Vec<&'a BootOption<B>>, PlatformError> {
    order
        .iter()
        .map(|entry| listed_option(entry, options))
        .collect()
}

/// The boot options `order` lists, in order, with every option `matches`
/// accepts moved to the front, the later ones first.
pub(crate) fn matching_first<'a, B: Bmc>(
    order: &[String],
    options: &'a [BootOption<B>],
    matches: impl Fn(&BootOption<B>) -> bool,
) -> Result<Vec<&'a BootOption<B>>, PlatformError> {
    let mut ordered = Vec::with_capacity(order.len());
    for option in listed_options(order, options)? {
        if matches(option) {
            ordered.insert(0, option);
        } else {
            ordered.push(option);
        }
    }
    Ok(ordered)
}

/// The device a request to boot one of the network, HTTP or disk devices
/// persistently names: a `Continuous` override with no mode or HTTP boot URI.
/// Platforms reorder their boot order for it rather than set an override.
pub(crate) fn persistent_device(setting: &BootUpdate) -> Option<BootSource> {
    device_for(setting, BootSourceOverrideEnabled::Continuous)
}

/// The device a request to boot one of the network, HTTP or disk devices
/// once names: a `Once` override with no mode or HTTP boot URI.
pub(crate) fn one_time_device(setting: &BootUpdate) -> Option<BootSource> {
    device_for(setting, BootSourceOverrideEnabled::Once)
}

fn device_for(setting: &BootUpdate, enabled: BootSourceOverrideEnabled) -> Option<BootSource> {
    if setting.boot_source_override_enabled != Some(enabled)
        || setting.boot_source_override_mode.is_some()
        || setting.http_boot_uri.is_some()
    {
        return None;
    }
    setting.boot_source_override_target.filter(|target| {
        matches!(
            target,
            BootSource::Pxe | BootSource::Hdd | BootSource::UefiHttp
        )
    })
}

/// Redfish boot discovery and writes shared by boot-order drivers.
pub(super) trait RedfishBootOrderExt<B: Bmc> {
    /// The selected system's boot options, in collection order.
    async fn boot_options(&self) -> Result<Vec<BootOption<B>>, PlatformError>;

    /// Writes `order` with the entry naming `reference` first as the system's
    /// own boot order, unless `order` already starts with it.
    async fn put_system_boot_option_first(
        &self,
        order: Vec<String>,
        reference: &str,
    ) -> Result<DriverOutcome, PlatformError>;

    /// The selected interface's MAC: the selector's own, or the one the system's
    /// Ethernet interface with the selector's id reports.
    async fn boot_interface_mac(
        &self,
        selector: &BootInterfaceSelector,
    ) -> Result<String, PlatformError>;

    /// Writes `update` to the system resource at `uri`, with `If-Match` set
    /// to `etag` or `*`.
    async fn patch_system(
        &self,
        uri: &ODataId,
        update: &ComputerSystemUpdate,
        etag: Option<&ODataETag>,
    ) -> Result<ModificationResponse<Value>, PlatformError>;

    /// Writes the override fields of `setting`, as [`boot_override`] builds
    /// them, to the system itself.
    async fn set_system_boot_override(
        &self,
        setting: &BootUpdate,
        uefi_by_default: bool,
    ) -> Result<DriverOutcome, PlatformError>;

    /// Stages `order` as the boot order on the settings object the system
    /// advertises, or on the system itself when it advertises none.
    async fn stage_boot_order(&self, order: Vec<String>) -> Result<DriverOutcome, PlatformError>;

    /// [`Self::stage_boot_order`] with the entry naming `reference` first,
    /// unless `order` already starts with it.
    async fn stage_boot_option_first(
        &self,
        order: Vec<String>,
        reference: &str,
    ) -> Result<DriverOutcome, PlatformError>;

    /// Stages the override fields of `setting`, as [`boot_override`] builds
    /// them, on the settings object the system advertises, or on the system
    /// itself when it advertises none.
    async fn stage_boot_override(
        &self,
        setting: &BootUpdate,
        uefi_by_default: bool,
    ) -> Result<DriverOutcome, PlatformError>;
}

impl<B: Bmc> RedfishBootOrderExt<B> for OpCx<'_, B> {
    async fn boot_options(&self) -> Result<Vec<BootOption<B>>, PlatformError> {
        self.system()
            .await?
            .boot_options()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .members()
            .await
            .map_err(|error| self.map_redfish_error(error))
    }

    async fn put_system_boot_option_first(
        &self,
        order: Vec<String>,
        reference: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        if is_first(&order, reference) {
            return Ok(DriverOutcome::complete());
        }
        let boot = BootUpdate::builder()
            .with_boot_order(with_first(order, reference))
            .build();
        update_system_boot(self, boot).await
    }

    async fn boot_interface_mac(
        &self,
        selector: &BootInterfaceSelector,
    ) -> Result<String, PlatformError> {
        let interface_id = match selector {
            BootInterfaceSelector::Mac(mac)
            | BootInterfaceSelector::Pair {
                mac_address: mac, ..
            } => return Ok(mac.to_string()),
            BootInterfaceSelector::InterfaceId(interface_id) => interface_id,
        };
        let interfaces = self
            .system()
            .await?
            .ethernet_interfaces()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .members()
            .await
            .map_err(|error| self.map_redfish_error(error))?;
        interfaces
            .iter()
            .find(|interface| interface.raw().id == *interface_id)
            .and_then(|interface| interface.mac_address())
            .map(|mac| mac.as_str().to_string())
            .filter(|mac| !mac.is_empty())
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: format!("EthernetInterface {interface_id} reports no MACAddress"),
            })
    }

    async fn patch_system(
        &self,
        uri: &ODataId,
        update: &ComputerSystemUpdate,
        etag: Option<&ODataETag>,
    ) -> Result<ModificationResponse<Value>, PlatformError> {
        self.bmc()
            .update(uri, etag, update)
            .await
            .map_err(|error| self.map_bmc_error(error))
    }

    async fn set_system_boot_override(
        &self,
        setting: &BootUpdate,
        uefi_by_default: bool,
    ) -> Result<DriverOutcome, PlatformError> {
        update_system_boot(self, boot_override(setting, uefi_by_default)?).await
    }

    async fn stage_boot_order(&self, order: Vec<String>) -> Result<DriverOutcome, PlatformError> {
        self.system()
            .await?
            .set_boot_order(order.into_iter().map(BootOptionReference::new).collect())
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error))
    }

    async fn stage_boot_option_first(
        &self,
        order: Vec<String>,
        reference: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        if is_first(&order, reference) {
            return Ok(DriverOutcome::complete());
        }
        self.stage_boot_order(with_first(order, reference)).await
    }

    async fn stage_boot_override(
        &self,
        setting: &BootUpdate,
        uefi_by_default: bool,
    ) -> Result<DriverOutcome, PlatformError> {
        let boot = boot_override(setting, uefi_by_default)?;
        self.system()
            .await?
            .set_boot_source_override(
                boot.boot_source_override_target
                    .ok_or(PlatformError::Unsupported)?,
                boot.boot_source_override_enabled
                    .ok_or(PlatformError::Unsupported)?,
                boot.boot_source_override_mode,
                boot.http_boot_uri,
            )
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error))
    }
}

/// The override fields of `setting`: target, enablement, the mode (UEFI when
/// `uefi_by_default` and the caller left it unset), and the HTTP boot URI;
/// `Unsupported` without a target or enablement.
pub(crate) fn boot_override(
    setting: &BootUpdate,
    uefi_by_default: bool,
) -> Result<BootUpdate, PlatformError> {
    let mode = match setting.boot_source_override_mode {
        None if uefi_by_default => Some(BootSourceOverrideMode::Uefi),
        mode => mode,
    };
    Ok(BootUpdate {
        boot_source_override_target: Some(
            setting
                .boot_source_override_target
                .ok_or(PlatformError::Unsupported)?,
        ),
        boot_source_override_enabled: Some(
            setting
                .boot_source_override_enabled
                .ok_or(PlatformError::Unsupported)?,
        ),
        boot_source_override_mode: mode,
        http_boot_uri: setting.http_boot_uri.clone(),
        ..BootUpdate::default()
    })
}

/// Writes `boot` to the selected system itself.
async fn update_system_boot<B: Bmc>(
    cx: &OpCx<'_, B>,
    boot: BootUpdate,
) -> Result<DriverOutcome, PlatformError> {
    cx.system()
        .await?
        .update(&ComputerSystemUpdate::builder().with_boot(boot).build())
        .await
        .map(DriverOutcome::from)
        .map_err(|error| cx.map_redfish_error(error))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    /// A `BootOrder` entry whose boot option the BMC does not list yet, as
    /// after a restart, is not ready rather than an invalid response.
    #[tokio::test]
    async fn an_entry_without_its_boot_option_is_not_ready() {
        const SYSTEM: &str = "/redfish/v1/Systems/System_0";
        const BOOT_OPTIONS: &str = "/redfish/v1/Systems/System_0/BootOptions";
        const OPTION: &str = "/redfish/v1/Systems/System_0/BootOptions/0002";
        let bmc = Fixture::new("NVIDIA", "GB BMC", "System_0", "BMC_0")
            .document(
                SYSTEM,
                json!({
                    "@odata.id": SYSTEM, "Id": "System_0", "Name": "System_0",
                    "Boot": {
                        "BootOrder": ["Boot0002", "Boot0008"],
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
                    "Members": [{"@odata.id": OPTION}],
                }),
            )
            .document(
                OPTION,
                json!({
                    "@odata.id": OPTION, "Id": "0002", "Name": "Boot0002",
                    "BootOptionReference": "Boot0002", "DisplayName": "ubuntu",
                }),
            )
            .build()
            .await;
        let cx = bmc.cx().await;
        let options = cx.boot_options().await.expect("boot options read");

        assert!(listed_option("Boot0002", &options).is_ok());
        assert!(matches!(
            listed_option("Boot0008", &options),
            Err(PlatformError::NotReady { .. })
        ));
    }

    #[test]
    fn only_a_bare_continuous_device_override_reorders_the_boot_order() {
        let setting =
            |target, enabled, mode: Option<BootSourceOverrideMode>, uri: Option<&str>| BootUpdate {
                boot_source_override_target: Some(target),
                boot_source_override_enabled: Some(enabled),
                boot_source_override_mode: mode,
                http_boot_uri: uri.map(str::to_string),
                ..BootUpdate::default()
            };
        let continuous = BootSourceOverrideEnabled::Continuous;
        let cases = [
            (
                "continuous disk",
                setting(BootSource::Hdd, continuous, None, None),
                Some(BootSource::Hdd),
            ),
            (
                "once",
                setting(BootSource::Pxe, BootSourceOverrideEnabled::Once, None, None),
                None,
            ),
            (
                "with a mode",
                setting(
                    BootSource::Pxe,
                    continuous,
                    Some(BootSourceOverrideMode::Uefi),
                    None,
                ),
                None,
            ),
            (
                "with an HTTP boot URI",
                setting(BootSource::UefiHttp, continuous, None, Some("http://boot")),
                None,
            ),
            (
                "another device",
                setting(BootSource::Cd, continuous, None, None),
                None,
            ),
        ];
        for (name, setting, expected) in cases {
            assert_eq!(persistent_device(&setting), expected, "{name}");
        }
    }

    #[test]
    fn promotion_keeps_the_reported_entry_and_drops_duplicates() {
        let order = |entries: &[&str]| entries.iter().map(|entry| (*entry).to_string()).collect();
        assert_eq!(
            with_first(
                order(&["Boot0001", "Boot0002: UEFI HTTPv4", "Boot0002"]),
                "Boot0002"
            ),
            order(&["Boot0002: UEFI HTTPv4", "Boot0001"])
        );
        assert_eq!(
            with_first(order(&["Boot0001"]), "Boot0003"),
            order(&["Boot0003", "Boot0001"])
        );
    }

    /// Settings writes go to the settings object the system advertises, and
    /// to the system itself when it advertises none, with the same body and
    /// `If-Match: *` either way.
    #[tokio::test]
    async fn settings_writes_follow_the_advertised_settings_object() {
        const SYSTEM: &str = "/redfish/v1/Systems/1";
        let override_setting = BootUpdate::builder()
            .with_boot_source_override_target(BootSource::Pxe)
            .with_boot_source_override_enabled(BootSourceOverrideEnabled::Once)
            .build();
        for (advertised, expected_path) in [
            (Some("/redfish/v1/Systems/1/SD"), "/redfish/v1/Systems/1/SD"),
            (None, SYSTEM),
        ] {
            let mut system = json!({"@odata.id": SYSTEM, "Id": "1", "Name": "System"});
            if let Some(settings) = advertised {
                system["@Redfish.Settings"] = json!({"SettingsObject": {"@odata.id": settings}});
            }
            let bmc = Fixture::new("NVIDIA", "GB BMC", "1", "1")
                .document(SYSTEM, system)
                .build()
                .await;
            let cx = bmc.cx().await;
            cx.stage_boot_order(vec!["Boot0002".to_string(), "Boot0001".to_string()])
                .await
                .expect("boot order stages");
            cx.stage_boot_override(&override_setting, true)
                .await
                .expect("override stages");
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
                [
                    (
                        expected_path.to_string(),
                        "*".to_string(),
                        json!({"Boot": {"BootOrder": ["Boot0002", "Boot0001"]}}),
                    ),
                    (
                        expected_path.to_string(),
                        "*".to_string(),
                        json!({"Boot": {
                            "BootSourceOverrideTarget": "Pxe",
                            "BootSourceOverrideEnabled": "Once",
                            "BootSourceOverrideMode": "UEFI",
                        }}),
                    ),
                ],
                "advertised: {advertised:?}"
            );
        }
    }
}
