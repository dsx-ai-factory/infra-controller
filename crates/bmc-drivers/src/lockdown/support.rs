/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Lockdown signal aggregation and host-interface controls shared by several
//! vendor drivers.

use std::fmt::Debug;

use bmc_platform::{DriverOutcome, LockdownState, LockdownStatus, OpCx, PlatformError};
use nv_redfish::core::Bmc;
use nv_redfish::host_interface::{HostInterface, HostInterfaceUpdate};

/// One control's observation: `(locked, unlocked)`, both false when unknown.
pub(super) type Signal = (bool, bool);

/// A control's value for a status message: as reported, or `unreported`.
pub(super) fn reported<T: Debug>(value: &Option<T>) -> String {
    match value {
        Some(value) => format!("{value:?}").trim_matches('"').to_string(),
        None => "unreported".to_string(),
    }
}

/// Reads a control whose two known values mean locked and unlocked.
pub(super) fn signal<T: PartialEq>(actual: Option<T>, locked: T, unlocked: T) -> Signal {
    (actual == Some(locked), actual == Some(unlocked))
}

/// Aggregates `(locked, unlocked)` observations of independent controls.
pub(super) fn state_from_signals(signals: &[Signal]) -> LockdownState {
    if signals
        .iter()
        .all(|(locked, unlocked)| !locked && !unlocked)
    {
        return LockdownState::Unknown;
    }
    if signals.iter().all(|(locked, _)| *locked) {
        LockdownState::Enabled
    } else if signals.iter().all(|(_, unlocked)| *unlocked) {
        LockdownState::Disabled
    } else {
        LockdownState::Partial
    }
}

pub(super) fn status(host: LockdownState, bmc: LockdownState, message: String) -> LockdownStatus {
    let aggregate = match (host, bmc) {
        (LockdownState::Unknown, LockdownState::Unknown) => LockdownState::Unknown,
        (LockdownState::Enabled, LockdownState::Enabled) => LockdownState::Enabled,
        (LockdownState::Disabled, LockdownState::Disabled) => LockdownState::Disabled,
        _ => LockdownState::Partial,
    };
    LockdownStatus {
        aggregate,
        message,
        host,
        bmc,
    }
}

/// Host-interface discovery and controls shared by lockdown drivers.
pub(super) trait RedfishLockdownExt<B: Bmc> {
    async fn host_interfaces(&self) -> Result<Vec<HostInterface<B>>, PlatformError>;

    /// BMC lockdown state derived from the manager's first host interface, which
    /// is enabled unless it reports otherwise.
    async fn host_interface_state(&self) -> Result<(LockdownState, Option<bool>), PlatformError>;

    /// Fails as [`Self::set_first_host_interface`] would when the manager
    /// lists no host interface to write.
    async fn require_host_interface(&self) -> Result<(), PlatformError>;

    async fn set_first_host_interface(&self, enabled: bool)
    -> Result<DriverOutcome, PlatformError>;

    /// Enables or disables one host interface.
    async fn set_host_interface(
        &self,
        interface: &HostInterface<B>,
        enabled: bool,
    ) -> Result<DriverOutcome, PlatformError>;
}

impl<B: Bmc> RedfishLockdownExt<B> for OpCx<'_, B> {
    async fn host_interfaces(&self) -> Result<Vec<HostInterface<B>>, PlatformError> {
        self.manager()
            .await?
            .host_interfaces()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .members()
            .await
            .map_err(|error| self.map_redfish_error(error))
    }

    async fn host_interface_state(&self) -> Result<(LockdownState, Option<bool>), PlatformError> {
        let enabled = self
            .host_interfaces()
            .await?
            .first()
            .map(|interface| interface.interface_enabled().unwrap_or(true));
        Ok((state_from_signals(&[signal(enabled, false, true)]), enabled))
    }

    async fn require_host_interface(&self) -> Result<(), PlatformError> {
        if self.host_interfaces().await?.is_empty() {
            return Err(PlatformError::NoContent);
        }
        Ok(())
    }

    async fn set_first_host_interface(
        &self,
        enabled: bool,
    ) -> Result<DriverOutcome, PlatformError> {
        let interface = self
            .host_interfaces()
            .await?
            .into_iter()
            .next()
            .ok_or(PlatformError::NoContent)?;
        self.set_host_interface(&interface, enabled).await
    }

    async fn set_host_interface(
        &self,
        interface: &HostInterface<B>,
        enabled: bool,
    ) -> Result<DriverOutcome, PlatformError> {
        let body = HostInterfaceUpdate::builder()
            .with_interface_enabled(enabled)
            .build();
        interface
            .update(&body)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error))
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use serde_json::json;

    use super::*;
    use crate::test_support::Fixture;

    #[tokio::test]
    async fn a_host_interface_that_omits_interface_enabled_counts_as_enabled() {
        const MANAGER: &str = "/redfish/v1/Managers/Self";
        const HOST_INTERFACES: &str = "/redfish/v1/Managers/Self/HostInterfaces";
        const HOST_INTERFACE: &str = "/redfish/v1/Managers/Self/HostInterfaces/Self";
        let bmc = Fixture::new("AMI", "AMI Redfish Server", "Self", "Self")
            .document(
                MANAGER,
                json!({
                    "@odata.id": MANAGER,
                    "Id": "Self",
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
                json!({"@odata.id": HOST_INTERFACE, "Id": "Self", "Name": "Host Interface"}),
            )
            .build()
            .await;
        let cx = bmc.cx().await;

        assert_eq!(
            cx.host_interface_state().await,
            Ok((LockdownState::Disabled, Some(true)))
        );
    }

    #[test]
    fn signals_aggregate_into_one_state() {
        value_scenarios!(run = |signals: Vec<(bool, bool)>| state_from_signals(&signals);
            "nothing reported" {
                vec![] => LockdownState::Unknown,
                vec![(false, false), (false, false)] => LockdownState::Unknown,
            }
            "all agree" {
                vec![(true, false), (true, false)] => LockdownState::Enabled,
                vec![(false, true), (false, true)] => LockdownState::Disabled,
            }
            "mixed" {
                vec![(true, false), (false, true)] => LockdownState::Partial,
            }
        );
    }

    #[test]
    fn host_and_bmc_states_that_differ_aggregate_to_partial() {
        assert_eq!(
            status(
                LockdownState::Enabled,
                LockdownState::Disabled,
                String::new()
            )
            .aggregate,
            LockdownState::Partial
        );
    }
}
