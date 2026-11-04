/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::BTreeMap;

use async_trait::async_trait;
use bmc_platform::{
    DriverOutcome, Lockdown, LockdownDesiredState, LockdownScope, LockdownStatus, OpCx,
    PlatformError,
};
use nv_redfish::core::Bmc;
use serde_json::Value;

use crate::lockdown::support::{
    RedfishLockdownExt as _, reported, signal, state_from_signals, status,
};
use crate::resources::RedfishResourcesExt as _;

/// A BIOS attribute whose two values mean locked and unlocked.
struct LockedAttribute {
    key: &'static str,
    locked: &'static str,
    unlocked: &'static str,
}

/// Generic AMI MegaRAC: host lockdown is KCS access and USB support in BIOS;
/// BMC lockdown is the manager's host interface, which a full change finds
/// before staging the BIOS so a BMC without one is left as it was.
pub(crate) struct MegaRacLockdown;

const HOST: &[LockedAttribute] = &[
    LockedAttribute {
        key: "KCSACP",
        locked: "Deny All",
        unlocked: "Allow All",
    },
    LockedAttribute {
        key: "USB000",
        locked: "Disabled",
        unlocked: "Enabled",
    },
];

#[async_trait]
impl<B: Bmc> Lockdown<B> for MegaRacLockdown {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<LockdownStatus, PlatformError> {
        let bios = cx.bios().await?;
        let values: Vec<(&str, Option<String>)> = HOST
            .iter()
            .map(|attribute| {
                let value = bios
                    .attribute(attribute.key)
                    .and_then(|value| value.str_value().map(str::to_owned));
                (attribute.key, value)
            })
            .collect();
        let signals: Vec<_> = HOST
            .iter()
            .zip(&values)
            .map(|(attribute, (_, value))| {
                signal(value.as_deref(), attribute.locked, attribute.unlocked)
            })
            .collect();
        let (bmc, interface) = cx.host_interface_state().await?;
        let mut message: Vec<String> = values
            .iter()
            .map(|(key, value)| format!("{key}={}", reported(value)))
            .collect();
        message.push(format!("host_interface={}", reported(&interface)));
        Ok(status(
            state_from_signals(&signals),
            bmc,
            message.join(", "),
        ))
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        scope: LockdownScope,
        desired: LockdownDesiredState,
    ) -> Result<DriverOutcome, PlatformError> {
        if scope == LockdownScope::BmcSystemLockdown {
            return Err(PlatformError::Unsupported);
        }
        let enabled = desired == LockdownDesiredState::Enabled;
        if scope == LockdownScope::All {
            cx.require_host_interface().await?;
        }
        let mut outcome = DriverOutcome::complete();
        if matches!(scope, LockdownScope::Host | LockdownScope::All) {
            let attributes: BTreeMap<String, Value> = HOST
                .iter()
                .map(|attribute| {
                    let value = if enabled {
                        attribute.locked
                    } else {
                        attribute.unlocked
                    };
                    (attribute.key.to_string(), Value::from(value))
                })
                .collect();
            outcome = outcome.merge(cx.stage_bios_attributes(&attributes).await?);
        }
        if matches!(scope, LockdownScope::Bmc | LockdownScope::All) {
            outcome = outcome.merge(cx.set_first_host_interface(!enabled).await?);
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, path};

    const MANAGER: &str = "/redfish/v1/Managers/Self";
    const HOST_INTERFACES: &str = "/redfish/v1/Managers/Self/HostInterfaces";
    const HOST_INTERFACE: &str = "/redfish/v1/Managers/Self/HostInterfaces/Self";
    const BIOS_SETTINGS: &str = "/redfish/v1/Systems/Self/Bios/Settings";

    /// A full lockdown writes nothing when the manager lists no host
    /// interface, rather than staging the BIOS half and then failing.
    #[tokio::test]
    async fn full_lockdown_writes_nothing_without_a_host_interface() {
        for (scenario, interfaces, expected) in [
            (
                "host interface listed",
                vec![json!({"@odata.id": HOST_INTERFACE})],
                Some(vec![BIOS_SETTINGS, HOST_INTERFACE]),
            ),
            ("no host interface", vec![], None),
        ] {
            let bmc = Fixture::new("AMI", "", "Self", "Self")
                .document(
                    MANAGER,
                    json!({"@odata.id": MANAGER, "Id": "Self", "Name": "Manager",
                           "HostInterfaces": {"@odata.id": HOST_INTERFACES}}),
                )
                .document(
                    HOST_INTERFACES,
                    json!({
                        "@odata.id": HOST_INTERFACES,
                        "@odata.type": "#HostInterfaceCollection.HostInterfaceCollection",
                        "Name": "Host Interface Collection",
                        "Members": interfaces,
                    }),
                )
                .document(
                    HOST_INTERFACE,
                    json!({"@odata.id": HOST_INTERFACE, "Id": "Self", "Name": "Host Interface",
                           "InterfaceEnabled": true}),
                )
                .bios(
                    "Self",
                    json!({"KCSACP": "Allow All", "USB000": "Enabled"}),
                    json!({}),
                )
                .build()
                .await;
            let cx = bmc.cx().await;

            let result = MegaRacLockdown
                .set(&cx, LockdownScope::All, LockdownDesiredState::Enabled)
                .await;
            let writes = bmc.writes();
            let written: Vec<&str> = writes.iter().map(path).collect();
            match expected {
                Some(expected) => {
                    result.unwrap_or_else(|error| panic!("{scenario}: {error}"));
                    assert_eq!(written, expected, "{scenario}");
                }
                None => {
                    assert!(
                        matches!(result, Err(PlatformError::NoContent)),
                        "{scenario}: {result:?}"
                    );
                    assert!(written.is_empty(), "{scenario}: {written:?}");
                }
            }
        }
    }
}
