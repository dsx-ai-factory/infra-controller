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

//! Firmware execution state shared by the local executor, Scout, and Core.

use serde::{Deserialize, Serialize};

use super::result::FirmwareFlashReport;

/// `FirmwareOperationState` describes execution, not permission to repeat a burn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirmwareOperationState {
    /// The request was recorded before any device work started.
    Accepted,
    /// The executor is inspecting, preparing, or changing the card.
    Running,
    /// Cancellation will be honored at the next safe phase boundary.
    CancelRequested,
    /// Every requested phase completed successfully.
    Complete,
    /// Execution stopped at a safe boundary; completed changes were not undone.
    Canceled,
    /// Execution was lost and the recorded effects need reconciliation.
    Interrupted,
    /// The available evidence does not establish a safe automatic next step.
    ManualIntervention,
    /// A known failure stopped execution; this does not authorize replay.
    Failed,
}

/// `FirmwareOperationPhase` identifies the step whose intent was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirmwareOperationPhase {
    /// Inspect the selected live card and its firmware identity.
    Inspect,
    /// Stage and check the required image and optional configuration.
    Prepare,
    /// Apply the optional device configuration.
    Configure,
    /// Burn the staged firmware image.
    Burn,
    /// Perform the requested activation/reset step.
    Activate,
    /// Compare the card with the staged image.
    VerifyImage,
    /// Compare the observed firmware version with the requested version.
    VerifyVersion,
}

/// `ObserverExitBehavior` applies when the submitting client process exits.
/// Closing an RPC/UDS connection or timing out an RPC never requests a stop;
/// process exit must be established separately by the lifecycle monitor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObserverExitBehavior {
    /// Continue the accepted operation when the client process exits (the default).
    #[default]
    Continue,
    /// Request a stop at the next safe boundary; never kill a mutation merely
    /// because the client process exited.
    Terminate,
}

/// `FirmwareOperation` is a receipt for one immutable submitted request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirmwareOperation {
    /// The caller's stable identifier, reused when submitting the same work.
    pub operation_id: String,
    /// Current execution or reconciliation state.
    pub state: FirmwareOperationState,
    /// The last recorded phase, or `None` before a phase was selected.
    pub phase: Option<FirmwareOperationPhase>,
    /// Completed firmware work; an incomplete report never permits replay.
    pub report: FirmwareFlashReport,
    /// Nonsecret diagnostic text, truncated to at most 2048 UTF-8 bytes.
    pub diagnostic: Option<String>,
}

impl FirmwareOperation {
    /// Whether execution completed and every reported required phase succeeded.
    /// Optional report fields are absent only for phases that were not requested.
    /// A completed no-work request has `flashed == false` because no burn ran.
    pub fn is_complete(&self) -> bool {
        self.state == FirmwareOperationState::Complete
            && self.report.reset.unwrap_or(true)
            && self.report.verified_image.unwrap_or(true)
            && self.report.verified_version.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Completion requires both a terminal execution state and successful
    /// requested phases; an omitted phase is not a failed phase.
    #[test]
    fn completion_requires_successful_requested_phases() {
        let success = FirmwareFlashReport {
            flashed: true,
            reset: Some(true),
            verified_image: Some(true),
            verified_version: Some(true),
            ..Default::default()
        };
        for (scenario, state, report, expected) in [
            // Successful requested phases must remain a usable terminal receipt.
            (
                "all requested phases succeeded",
                FirmwareOperationState::Complete,
                success.clone(),
                true,
            ),
            // No selected profile completes without claiming a hardware effect.
            (
                "no-work completion does not invent a burn",
                FirmwareOperationState::Complete,
                FirmwareFlashReport::default(),
                true,
            ),
            // Partial progress cannot advance Core before execution settles.
            (
                "success fields do not complete running work",
                FirmwareOperationState::Running,
                success.clone(),
                false,
            ),
            // Activation is required when its result field is present.
            (
                "requested activation failed",
                FirmwareOperationState::Complete,
                FirmwareFlashReport {
                    reset: Some(false),
                    ..success.clone()
                },
                false,
            ),
            // A completed burn cannot hide a requested image comparison failure.
            (
                "requested image comparison failed",
                FirmwareOperationState::Complete,
                FirmwareFlashReport {
                    verified_image: Some(false),
                    ..success.clone()
                },
                false,
            ),
            // Version mismatch must not be treated as successful convergence.
            (
                "requested version comparison failed",
                FirmwareOperationState::Complete,
                FirmwareFlashReport {
                    verified_version: Some(false),
                    ..success
                },
                false,
            ),
        ] {
            let operation = FirmwareOperation {
                operation_id: "firmware-attempt".to_string(),
                state,
                phase: None,
                report,
                diagnostic: None,
            };
            assert_eq!(operation.is_complete(), expected, "{scenario}");
        }
    }
}
