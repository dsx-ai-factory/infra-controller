/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{
    DriverOutcome, Lockdown, LockdownDesiredState, LockdownScope, LockdownState, LockdownStatus,
    OpCx, PlatformError,
};
use nv_redfish::core::Bmc;
use nv_redfish::host_interface::{HostInterface, HostInterfaceUpdate};
use nv_redfish::schema::host_interface::{AuthenticationMode, CredentialBootstrappingUpdate};

use crate::lockdown::support::{RedfishLockdownExt as _, Signal, signal, state_from_signals};

/// Giga Computing AMI lockdown, as on R263-ZG0.
///
/// The host reaches the BMC without credentials over a host interface,
/// through `AuthNone` or by bootstrapping credentials, so full lockdown
/// disables every interface and closes both, then reads them back: the BMC
/// can accept a write it does not apply. The BMC scope toggles only the
/// interfaces, around configuration changes. KCS can be neither set nor read
/// over Redfish here, so the host side reads unknown and lockdown leaves KCS
/// as it is.
pub(crate) struct GigaComputingAmiLockdown;

/// The host interface controls lockdown sets; one the BMC does not report
/// reads as open.
struct Controls {
    interface_enabled: bool,
    auth_none: bool,
    credential_bootstrapping: bool,
}

impl Controls {
    fn read<B: Bmc>(interface: &HostInterface<B>) -> Self {
        let raw = interface.raw();
        Self {
            interface_enabled: interface.interface_enabled().unwrap_or(true),
            auth_none: raw.authentication_modes.as_ref().is_none_or(|modes| {
                modes
                    .iter()
                    .any(|mode| matches!(mode, AuthenticationMode::AuthNone))
            }),
            credential_bootstrapping: raw
                .credential_bootstrapping
                .as_ref()
                .and_then(|bootstrapping| bootstrapping.enabled.flatten())
                .unwrap_or(true),
        }
    }

    fn signals(&self) -> [Signal; 3] {
        [
            signal(Some(self.interface_enabled), false, true),
            signal(Some(self.auth_none), false, true),
            signal(Some(self.credential_bootstrapping), false, true),
        ]
    }
}

/// Every host interface's controls, read afresh.
async fn read_all<B: Bmc>(cx: &OpCx<'_, B>) -> Result<Vec<Controls>, PlatformError> {
    Ok(cx
        .host_interfaces()
        .await?
        .iter()
        .map(Controls::read)
        .collect())
}

/// Locked only when every interface is shut; a BMC listing no interface is
/// not locked down.
fn state(interfaces: &[Controls]) -> LockdownState {
    if interfaces.is_empty() {
        return LockdownState::Partial;
    }
    let signals: Vec<Signal> = interfaces.iter().flat_map(Controls::signals).collect();
    state_from_signals(&signals)
}

fn message(interfaces: &[Controls]) -> String {
    let read = if interfaces.is_empty() {
        "no host interfaces found".to_string()
    } else {
        interfaces
            .iter()
            .map(|controls| {
                format!(
                    "host_interface={}, auth_none={}, cred_bootstrap={}",
                    controls.interface_enabled,
                    controls.auth_none,
                    controls.credential_bootstrapping
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    format!("{read}; KCS not exposed via Redfish on this platform (state unknown)")
}

#[async_trait]
impl<B: Bmc> Lockdown<B> for GigaComputingAmiLockdown {
    async fn status(&self, cx: &OpCx<'_, B>) -> Result<LockdownStatus, PlatformError> {
        let interfaces = read_all(cx).await?;
        let bmc = state(&interfaces);
        Ok(LockdownStatus {
            aggregate: bmc,
            message: message(&interfaces),
            host: LockdownState::Unknown,
            bmc,
        })
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        scope: LockdownScope,
        desired: LockdownDesiredState,
    ) -> Result<DriverOutcome, PlatformError> {
        let locking = desired == LockdownDesiredState::Enabled;
        match scope {
            LockdownScope::All => {}
            LockdownScope::Bmc => {
                let mut outcome = DriverOutcome::complete();
                for interface in cx.host_interfaces().await? {
                    outcome = outcome.merge(cx.set_host_interface(&interface, !locking).await?);
                }
                return Ok(outcome);
            }
            LockdownScope::Host | LockdownScope::BmcSystemLockdown => {
                return Err(PlatformError::Unsupported);
            }
        }
        let mode = if locking {
            AuthenticationMode::RedfishSessionAuth
        } else {
            AuthenticationMode::AuthNone
        };
        let update = HostInterfaceUpdate::builder()
            .with_interface_enabled(!locking)
            .with_authentication_modes(vec![mode])
            .with_credential_bootstrapping(
                CredentialBootstrappingUpdate::builder()
                    .with_enabled(!locking)
                    .build(),
            )
            .build();
        let mut outcome = DriverOutcome::complete();
        for interface in cx.host_interfaces().await? {
            outcome = outcome.merge(
                interface
                    .update(&update)
                    .await
                    .map(DriverOutcome::from)
                    .map_err(|error| cx.map_redfish_error(error))?,
            );
        }
        let interfaces = read_all(cx).await?;
        let target = if locking {
            LockdownState::Enabled
        } else {
            LockdownState::Disabled
        };
        if state(&interfaces) != target {
            return Err(PlatformError::InvalidResponse {
                message: format!(
                    "the host interfaces did not take the lockdown change: {}",
                    message(&interfaces)
                ),
            });
        }
        Ok(outcome)
    }
}
