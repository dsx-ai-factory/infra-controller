/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Shared onboard NIC toggle sequence for ingestion and AC-cycle recovery.

use chrono::Duration;
use model::machine::{DisableOnboardNicsState, ManagedHostState, ManagedHostStateSnapshot};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::redfish_error;
use crate::context::MachineStateHandlerContextObjects;

pub(super) enum Progress {
    Wait,
    Next(DisableOnboardNicsState),
    Complete,
}

pub(super) fn wait() -> StateHandlerOutcome<ManagedHostState> {
    StateHandlerOutcome::wait("Waiting for onboard NIC configuration to be applied".to_string())
}

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
            let target = if *enable {
                libredfish::EnabledDisabled::Enabled
            } else {
                libredfish::EnabledDisabled::Disabled
            };
            client
                .set_enable_onboard_nics(target)
                .await
                .map_err(|error| {
                    redfish_error(
                        if *enable {
                            "enable_onboard_nics"
                        } else {
                            "disable_onboard_nics"
                        },
                        error,
                    )
                })?;
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
