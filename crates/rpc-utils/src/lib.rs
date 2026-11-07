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

pub mod dhcp;
pub mod machine_discovery;
pub mod managed_host_display;

pub use managed_host_display::{ManagedHostMetadata, ManagedHostOutput, get_managed_host_output};

/// A string to display to the user. Either the 'reason' or 'err' field, or None.
pub fn reason_to_user_string(p: &rpc::forge::ControllerStateReason) -> Option<String> {
    use rpc::forge::ControllerStateOutcome::*;
    let Ok(outcome) = rpc::forge::ControllerStateOutcome::try_from(p.outcome) else {
        tracing::error!("Invalid rpc::forge::ControllerStateOutcome i32, should be impossible.");
        return None;
    };
    match outcome {
        Transition | DoNothing | Todo => None,
        Wait | Error => p.outcome_msg.clone(),
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use rpc::forge::{ControllerStateOutcome, ControllerStateReason};

    use super::*;

    fn reason(outcome: ControllerStateOutcome, message: Option<&str>) -> ControllerStateReason {
        ControllerStateReason {
            outcome: outcome as i32,
            outcome_msg: message.map(str::to_string),
            source_ref: None,
        }
    }

    #[test]
    fn maps_controller_reasons_to_user_strings() {
        value_scenarios!(
            run = |reason| reason_to_user_string(&reason);
            "visible outcomes" {
                reason(ControllerStateOutcome::Wait, Some("waiting for host")) => Some("waiting for host".to_string()),
                reason(ControllerStateOutcome::Error, Some("firmware failed")) => Some("firmware failed".to_string()),
                reason(ControllerStateOutcome::Wait, None) => None,
            }

            "hidden outcomes" {
                reason(ControllerStateOutcome::Transition, Some("transitioning")) => None,
                reason(ControllerStateOutcome::DoNothing, Some("no-op")) => None,
                reason(ControllerStateOutcome::Todo, Some("todo")) => None,
            }

            "invalid outcome" {
                ControllerStateReason {
                    outcome: 999,
                    outcome_msg: Some("ignored".to_string()),
                    source_ref: None,
                } => None,
            }
        );
    }
}
