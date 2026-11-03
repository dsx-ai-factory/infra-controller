/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    DriverOutcome, Lockdown, LockdownDesiredState, LockdownScope, LockdownState, LockdownStatus,
    OpCx, PlatformError,
};
use nv_redfish::core::{ActionError, Bmc};
use serde_json::json;

use crate::dell::{self, ManagerApplyTime};
use crate::lockdown::support::{reported, signal, state_from_signals};
use crate::resources::{RedfishResourcesExt as _, attribute_map, bios_update};

/// Dell iDRAC lockdown driver.
///
/// Lockdown is the iDRAC system-lockdown switch with `racadm` disabled and the
/// first boot device pinned. The BIOS in-band and UEFI-variable restrictions
/// block IPMI, so they are staged only for an explicit host lockdown and are
/// always lifted on a full unlock; the aggregate follows the BMC, except that
/// an unlock is done only once UEFI variable access is lifted too.
pub(crate) struct IdracLockdown;

/// The aggregate of `bmc` and the UEFI variable restriction `uefi`. Locking
/// leaves the host restrictions open, while a full unlock lifts them; only
/// `UefiVariableAccess` counts, since it blocks BIOS changes, while BIOS
/// setup disables the in-band interface itself.
fn aggregate(bmc: LockdownState, uefi: LockdownState) -> LockdownState {
    match (bmc, uefi) {
        (LockdownState::Disabled, LockdownState::Enabled) => LockdownState::Partial,
        (bmc, _) => bmc,
    }
}

/// The device lockdown pins first; the XE9680 cannot PXE boot.
async fn first_boot_device<B: Bmc>(cx: &OpCx<'_, B>) -> Result<&'static str, PlatformError> {
    let model = cx.system().await?.raw().model.clone().flatten();
    Ok(match model.as_deref() {
        Some("PowerEdge XE9680") => "UefiHttp",
        _ => "PXE",
    })
}

async fn set_host<B: Bmc>(cx: &OpCx<'_, B>, enabled: bool) -> Result<DriverOutcome, PlatformError>
where
    B::Error: ActionError,
{
    dell::stage_bios_attributes(
        cx,
        &attribute_map([
            (
                "InBandManageabilityInterface",
                json!(if enabled { "Disabled" } else { "Enabled" }),
            ),
            (
                "UefiVariableAccess",
                json!(if enabled { "Controlled" } else { "Standard" }),
            ),
        ]),
    )
    .await
}

/// Lifts the BIOS restrictions on the next reset; some iDRACs report them
/// read-only, which leaves nothing to lift.
async fn unlock_bios<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
    let body = bios_update(&attribute_map([
        ("InBandManageabilityInterface", json!("Enabled")),
        ("UefiVariableAccess", json!("Standard")),
    ]))?
    .with_settings_apply_time(dell::on_reset());
    match cx.update_bios_settings(&body).await {
        Ok(response) => dell::job_outcome(cx, response).await,
        Err(error) if dell::is_read_only_attribute(&error) => Ok(DriverOutcome::complete()),
        Err(error) => Err(error),
    }
}

/// System lockdown takes effect at once and rejects later writes, so the
/// other restrictions are staged first.
async fn lock_bmc<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
    let boot_device = first_boot_device(cx).await?;
    let restriction_attributes = attribute_map([
        ("Racadm.1.Enable", json!("Disabled")),
        ("ServerBoot.1.FirstBootDevice", json!(boot_device)),
        ("ServerBoot.1.BootOnce", json!("Disabled")),
    ]);
    let lockdown_attributes = attribute_map([(dell::SYSTEM_LOCKDOWN, json!("Enabled"))]);
    let on_reset = Some(ManagerApplyTime::OnReset);
    let restrictions =
        dell::patch_manager_attributes(cx, &restriction_attributes, on_reset).await?;
    let lockdown = dell::patch_manager_attributes(cx, &lockdown_attributes, on_reset).await?;
    Ok(restrictions.merge(lockdown))
}

/// Flips only the system-lockdown switch, which takes effect at once.
async fn set_system_lockdown<B: Bmc>(
    cx: &OpCx<'_, B>,
    enabled: bool,
) -> Result<DriverOutcome, PlatformError> {
    let attributes = attribute_map([(
        dell::SYSTEM_LOCKDOWN,
        json!(if enabled { "Enabled" } else { "Disabled" }),
    )]);
    dell::patch_manager_attributes(cx, &attributes, None).await
}

async fn unlock_bmc<B: Bmc>(cx: &OpCx<'_, B>) -> Result<DriverOutcome, PlatformError> {
    let boot_device = first_boot_device(cx).await?;
    let attributes = attribute_map([
        (dell::SYSTEM_LOCKDOWN, json!("Disabled")),
        ("Racadm.1.Enable", json!("Enabled")),
        ("ServerBoot.1.FirstBootDevice", json!(boot_device)),
        ("ServerBoot.1.BootOnce", json!("Disabled")),
    ]);
    dell::patch_manager_attributes(cx, &attributes, Some(ManagerApplyTime::Immediate)).await
}

#[async_trait]
impl<B: Bmc> Lockdown<B> for IdracLockdown
where
    B::Error: ActionError,
{
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<LockdownStatus, PlatformError> {
        let bios = cx.bios().await?;
        let in_band = bios
            .attribute("InBandManageabilityInterface")
            .and_then(|value| value.str_value().map(str::to_owned));
        let uefi = bios
            .attribute("UefiVariableAccess")
            .and_then(|value| value.str_value().map(str::to_owned));
        let uefi_restricted = signal(uefi.as_deref(), "Controlled", "Standard");
        let host = state_from_signals(&[
            signal(in_band.as_deref(), "Disabled", "Enabled"),
            uefi_restricted,
        ]);

        let attrs = dell::manager_attributes(cx).await?;
        let system_lockdown = attrs
            .attribute(dell::SYSTEM_LOCKDOWN)
            .and_then(|value| value.str_value().map(str::to_owned));
        let racadm = attrs
            .attribute("Racadm.1.Enable")
            .and_then(|value| value.str_value().map(str::to_owned));
        let bmc = state_from_signals(&[
            signal(system_lockdown.as_deref(), "Enabled", "Disabled"),
            signal(racadm.as_deref(), "Disabled", "Enabled"),
        ]);
        Ok(LockdownStatus {
            aggregate: aggregate(bmc, state_from_signals(&[uefi_restricted])),
            message: format!(
                "in_band={}, uefi_variable_access={}, system_lockdown={}, racadm={}",
                reported(&in_band),
                reported(&uefi),
                reported(&system_lockdown),
                reported(&racadm)
            ),
            host,
            bmc,
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
            LockdownScope::BmcSystemLockdown => set_system_lockdown(cx, enabled).await,
            LockdownScope::Bmc | LockdownScope::All if enabled => lock_bmc(cx).await,
            LockdownScope::Bmc => unlock_bmc(cx).await,
            LockdownScope::All => {
                let bmc = unlock_bmc(cx).await?;
                Ok(bmc.merge(unlock_bios(cx).await?))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const MANAGER: &str = "/redfish/v1/Managers/iDRAC.Embedded.1";
    const ATTRIBUTES: &str =
        "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1";

    /// A full unlock is done only once UEFI variable access is lifted too;
    /// locking leaves it open, so the BMC alone decides it.
    #[test]
    fn an_unlock_waits_for_uefi_variable_access() {
        use LockdownState::{Disabled, Enabled, Partial, Unknown};
        for (scenario, bmc, uefi, expected) in [
            (
                "BMC unlocked, UEFI variables controlled",
                Disabled,
                Enabled,
                Partial,
            ),
            ("fully unlocked", Disabled, Disabled, Disabled),
            (
                "BMC unlocked, UEFI access unread",
                Disabled,
                Unknown,
                Disabled,
            ),
            ("locked, UEFI variables open", Enabled, Disabled, Enabled),
        ] {
            assert_eq!(aggregate(bmc, uefi), expected, "{scenario}");
        }
    }

    #[tokio::test]
    async fn system_lockdown_scope_flips_only_the_switch_at_once() {
        for (desired, value) in [
            (LockdownDesiredState::Enabled, "Enabled"),
            (LockdownDesiredState::Disabled, "Disabled"),
        ] {
            let bmc = Fixture::new(
                "Dell",
                "Integrated Dell Remote Access Controller",
                "System.Embedded.1",
                "iDRAC.Embedded.1",
            )
            .document(
                MANAGER,
                json!({
                    "@odata.id": MANAGER,
                    "Id": "iDRAC.Embedded.1",
                    "Name": "Manager",
                    "Links": {"Oem": {"Dell": {"DellAttributes": [{"@odata.id": ATTRIBUTES}]}}},
                }),
            )
            .document(
                ATTRIBUTES,
                json!({
                    "@odata.id": ATTRIBUTES,
                    "Id": "iDRAC.Embedded.1",
                    "Name": "Manager Attributes",
                    "Attributes": {dell::SYSTEM_LOCKDOWN: "Disabled"},
                }),
            )
            .build()
            .await;
            let cx = bmc.cx().await;

            IdracLockdown
                .set(&cx, LockdownScope::BmcSystemLockdown, desired)
                .await
                .expect("system lockdown changes");

            let writes = bmc.writes();
            assert_eq!(writes.len(), 1, "{value}");
            assert_eq!(writes[0].method, Method::PATCH, "{value}");
            assert_eq!(path(&writes[0]), ATTRIBUTES, "{value}");
            assert_eq!(
                body(&writes[0]),
                json!({"Attributes": {dell::SYSTEM_LOCKDOWN: value}}),
                "{value}"
            );
        }
    }
}
