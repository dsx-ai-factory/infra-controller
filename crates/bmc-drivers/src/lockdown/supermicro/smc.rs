/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    DriverOutcome, Lockdown, LockdownDesiredState, LockdownScope, LockdownStatus, OpCx,
    PlatformError, Quirk,
};
use nv_redfish::core::Bmc;
use nv_redfish::oem::supermicro::kcs_interface::Privilege;

use crate::lockdown::supermicro::support::{
    host_interface_enabled, kcs_privilege, kcs_signal, set_host_interfaces, set_kcs_privilege,
    set_sys_lockdown, sys_lockdown, sys_lockdown_scope,
};
use crate::lockdown::support::{reported, signal, state_from_signals, status};

/// Supermicro lockdown driver.
///
/// Host lockdown disables the host interfaces and then drops KCS to callback
/// privilege, and unlocking reverses that order; BMC lockdown is the OEM
/// `SysLockdown` switch, which must be cleared before any other write and set
/// after them. With [`Quirk::SupermicroHostInterfaceRequired`] the host
/// interfaces stay up while locked and are only re-enabled on unlock.
pub(crate) struct SmcLockdown;

#[async_trait]
impl<B: Bmc> Lockdown<B> for SmcLockdown {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<LockdownStatus, PlatformError> {
        let lockdown = sys_lockdown(cx).await?;
        let privilege = kcs_privilege(cx).await?;
        let host_interface = host_interface_enabled(cx).await?;

        let host_interface_signal = if cx.has_quirk(Quirk::SupermicroHostInterfaceRequired) {
            // The interface stays up while locked, so it only gates unlocking.
            (true, host_interface == Some(true))
        } else {
            signal(host_interface, false, true)
        };
        let host = state_from_signals(&[kcs_signal(privilege), host_interface_signal]);
        let bmc = state_from_signals(&[signal(lockdown, true, false)]);
        Ok(status(
            host,
            bmc,
            format!(
                "sys_lockdown={}, kcs={}, host_interface={}",
                reported(&lockdown),
                reported(&privilege),
                reported(&host_interface)
            ),
        ))
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        scope: LockdownScope,
        desired: LockdownDesiredState,
    ) -> Result<DriverOutcome, PlatformError> {
        let sys_lockdown = sys_lockdown_scope(scope)?;
        let enabled = desired == LockdownDesiredState::Enabled;
        let mut outcome = DriverOutcome::complete();
        if !enabled && sys_lockdown {
            outcome = outcome.merge(set_sys_lockdown(cx, false).await?);
        }
        if matches!(scope, LockdownScope::Host | LockdownScope::All) {
            if enabled {
                if !cx.has_quirk(Quirk::SupermicroHostInterfaceRequired) {
                    outcome = outcome.merge(set_host_interfaces(cx, false).await?);
                }
                outcome = outcome.merge(set_kcs_privilege(cx, Privilege::Callback).await?);
            } else {
                outcome = outcome.merge(set_kcs_privilege(cx, Privilege::Administrator).await?);
                outcome = outcome.merge(set_host_interfaces(cx, true).await?);
            }
        }
        if enabled && sys_lockdown {
            outcome = outcome.merge(set_sys_lockdown(cx, true).await?);
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, path};

    const MANAGER: &str = "/redfish/v1/Managers/1";
    const HOST_INTERFACES: &str = "/redfish/v1/Managers/1/HostInterfaces";
    const HOST_INTERFACE: &str = "/redfish/v1/Managers/1/HostInterfaces/1";
    const SYSTEM: &str = "/redfish/v1/Systems/1";

    #[tokio::test]
    async fn host_lockdown_leaves_a_required_host_interface_up() {
        let mgx_c2 = [Quirk::SupermicroMgxC2, Quirk::SupermicroIpmiHostInterface];
        for (scenario, required, expected) in [
            (
                "host interface optional",
                false,
                vec![HOST_INTERFACE, SYSTEM],
            ),
            ("host interface required", true, vec![SYSTEM]),
        ] {
            let bmc = Fixture::new("Supermicro", "", "1", "1")
                .document(
                    MANAGER,
                    json!({
                        "@odata.id": MANAGER,
                        "Id": "1",
                        "Name": "Manager",
                        "HostInterfaces": {"@odata.id": HOST_INTERFACES},
                    }),
                )
                .document(
                    HOST_INTERFACES,
                    json!({
                        "@odata.id": HOST_INTERFACES,
                        "@odata.type": "#HostInterfaceCollection.HostInterfaceCollection",
                        "Name": "Host Interface Collection",
                        "Members": [{"@odata.id": HOST_INTERFACE}],
                    }),
                )
                .document(
                    HOST_INTERFACE,
                    json!({
                        "@odata.id": HOST_INTERFACE,
                        "Id": "1",
                        "Name": "Host Interface",
                        "InterfaceEnabled": true,
                    }),
                )
                .build()
                .await;
            let mut quirks = BTreeSet::from(mgx_c2);
            if required {
                quirks.insert(Quirk::SupermicroHostInterfaceRequired);
            }
            let cx = bmc.cx().await.with_quirks(&quirks);

            SmcLockdown
                .set(&cx, LockdownScope::Host, LockdownDesiredState::Enabled)
                .await
                .expect("host lockdown succeeds");

            assert_eq!(
                bmc.writes().iter().map(path).collect::<Vec<_>>(),
                expected,
                "{scenario}"
            );
        }
    }
}
