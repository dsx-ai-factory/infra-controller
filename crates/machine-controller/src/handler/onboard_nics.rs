// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use chrono::Duration;
use model::machine::{DisableOnboardNicsState, ManagedHostStateSnapshot};
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use crate::context::MachineStateHandlerContextObjects;

pub(super) enum Progress {
    Wait,
    Next(DisableOnboardNicsState),
    Complete,
}

pub(super) const WAIT_REASON: &str = "Waiting for onboard NIC configuration to be applied";

/// Shared ingestion and post-AC-cycle sequence: enable, wait, disable, wait.
/// Each successful write advances persisted state; errors retry only that write.
pub(super) async fn advance(
    state: &DisableOnboardNicsState,
    snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Progress, StateHandlerError> {
    match state {
        DisableOnboardNicsState::ConfigureOnboardNics { enable } => {
            let client = ctx
                .services
                .create_redfish_client_from_machine(&snapshot.host_snapshot)
                .await?;
            let (operation, target) = if *enable {
                ("enable_onboard_nics", libredfish::EnabledDisabled::Enabled)
            } else {
                (
                    "disable_onboard_nics",
                    libredfish::EnabledDisabled::Disabled,
                )
            };
            client
                .set_enable_onboard_nics(target)
                .await
                .map_err(|error| redfish_error(operation, error))?;
            Ok(Progress::Next(
                DisableOnboardNicsState::WaitForConfigToBeApplied {
                    onboard_nics_disabled: !enable,
                },
            ))
        }
        DisableOnboardNicsState::WaitForConfigToBeApplied {
            onboard_nics_disabled,
        } => {
            if snapshot.host_snapshot.state.version.since_state_change() < Duration::seconds(10) {
                Ok(Progress::Wait)
            } else if *onboard_nics_disabled {
                Ok(Progress::Complete)
            } else {
                Ok(Progress::Next(
                    DisableOnboardNicsState::ConfigureOnboardNics { enable: false },
                ))
            }
        }
    }
}
