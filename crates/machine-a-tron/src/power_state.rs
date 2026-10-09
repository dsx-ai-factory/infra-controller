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
use bmc_mock::{ActionError, ResourceResetType};
use nv_redfish::schema::resource::PowerState;

/// Validates reset requests against the latest FSM power observation.
pub(crate) fn validate_reset_type(
    power_state: PowerState,
    reset_type: ResourceResetType,
) -> Result<(), ActionError> {
    use ResourceResetType::*;
    match (reset_type, power_state) {
        (GracefulShutdown | ForceOff | GracefulRestart | ForceRestart, PowerState::Off) => {
            Err(ActionError::BadRequest(eyre::eyre!(
                "bmc-mock: cannot power off machine, it is already off",
            )))
        }
        (On | ForceOn, PowerState::On | PowerState::PoweringOn) => Err(ActionError::BadRequest(
            eyre::eyre!("bmc-mock: cannot power on machine, it is already on",),
        )),
        (On | ForceOn, PowerState::PoweringOff) => Err(ActionError::BadRequest(eyre::eyre!(
            "bmc-mock: cannot power on machine, it is shutting down",
        ))),
        _ => Ok(()),
    }
}
