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

//! Durable NIC firmware progress for the managed-host controller.
//!
//! An admitted [`NicFirmwareOperation`] freezes its cards, firmware requests
//! and execution choices. Supervisor receipts describe what execution did;
//! fresh ordinary inventory establishes that a card runs the requested version.

use carbide_libmlx_model::firmware::operation::{
    FirmwareOperation, FirmwareOperationPhase, FirmwareOperationState,
};
use carbide_uuid::instance::InstanceId;
use chrono::{DateTime, Utc};
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use super::CleanupContext;
use crate::ConfigValidationError;
use crate::nic_firmware::{
    NicFirmwareCardSelector, NicFirmwareProfileConfig, NicFirmwareResolution,
};
use crate::pci::PciBdfOrderingKey;

#[cfg(test)]
mod tests;

/// One admitted host workflow, processing its frozen targets in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicFirmwareOperation {
    /// Stable workflow identity: 1..=256 printable nonspace ASCII bytes,
    /// without the `dpa-firmware:` prefix reserved for DPA state entries.
    /// Every card's request uses it as both the supervisor operation ID and
    /// the workflow phase ID. Each PCI slot keeps its own supervisor journal,
    /// so one ID is enough to tell the cards apart.
    pub operation_id: String,
    /// The lifecycle that admitted this workflow and where it resumes.
    pub purpose: NicFirmwarePurpose,
    /// Nonempty ordered physical targets supplied by admission; never re-resolved.
    pub targets: Vec<NicFirmwareTarget>,
    /// Execution choices sent unchanged with every card's request.
    pub options: NicFirmwareExecutionOptions,
    /// Where the workflow stands. A step must be stored before its commands go
    /// out, so a restart resumes from it.
    pub phase: NicFirmwarePhase,
    /// The current lockdown command and its accepted report. A phase or card
    /// transition must start over with a status read.
    pub lockdown: NicFirmwareLockdownProgress,
}

/// The lifecycle that admitted the workflow and owns its successful continuation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicFirmwarePurpose {
    /// Establish or update the tenant-free baseline.
    Baseline {
        /// Existing host preparation or idle state to resume.
        continuation: NicFirmwareBaselineContinuation,
    },
    /// Apply the firmware requirement frozen for one allocation, then resume
    /// `InstanceState::DpaProvisioning`.
    Allocation {
        /// Allocation that must still own the host at completion.
        instance_id: InstanceId,
    },
    /// Return the cards to the baseline that applies at release, then resume
    /// `CleanupState::Init` with the original cleanup context.
    RestoreBaseline {
        /// Allocation whose cleanup owns this work.
        instance_id: InstanceId,
        /// Existing cleanup reason, preserved across firmware work.
        cleanup_context: CleanupContext,
    },
}

/// Supported tenant-free continuation points, without arbitrary host states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicFirmwareBaselineContinuation {
    /// Resume the discovered host's existing boot configuration and preparation.
    InitialPreparation {
        /// Preserve the discovered host's existing reboot-wait choice.
        skip_reboot_wait: bool,
    },
    /// Return an idle, already prepared host to the allocation pool.
    Ready,
}

/// A frozen card request and the evidence accepted for that exact request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicFirmwareTarget {
    /// Stable card identity. Scout checks it against the live card before any
    /// mutation, and later inventory must report it too.
    pub selector: NicFirmwareCardSelector,
    /// Full lowercase PCI BDF (`dddd:bb:dd.f`) naming this card's supervisor
    /// journal for every retry.
    pub pci_name: String,
    /// Exact selected profile revision, hardware pair, source URLs and digests.
    pub resolution: NicFirmwareResolution,
    /// Lockdown policy read before the update and restored afterward,
    /// including the original key context.
    pub original_lockdown: NicFirmwareLockdownPolicy,
    /// Helper instance first observed without this card's receipt. Never
    /// refreshed once set, so a replacement helper that lost its journal
    /// cannot authorize a new burn. Without it, the card can only be observed:
    /// `validate` rejects `Unlocking` and `UpdatingFirmware` for a card that has
    /// neither this value nor a receipt. Present values contain 1..=256
    /// printable nonspace ASCII bytes.
    pub supervisor_instance_id: Option<String>,
    /// Last accepted correlated receipt; a terminal receipt cannot regress.
    pub receipt: Option<FirmwareOperation>,
    /// Latest diagnostic from a correlated execution report. A new receipt or
    /// the first helper instance clears it.
    pub diagnostic: Option<String>,
}

/// Execution choices frozen at admission. Core sends them unchanged with every
/// card's request, so a retry keeps the supervisor's request identity; omitted
/// timing stays absent for the same reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicFirmwareExecutionOptions {
    /// Ask the supervisor to compare each burned card with the staged image.
    pub verify_image: bool,
    /// Ask the supervisor to read back each card's exact requested version.
    pub verify_version: bool,
    /// How each burned card starts running its new firmware.
    pub activation: NicFirmwareActivation,
}

/// How burned cards start running their new firmware. One choice covers every
/// card in the operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicFirmwareActivation {
    /// The supervisor resets each card after its burn, then runs any requested
    /// verification.
    CardReset {
        /// MFT reset level passed to `mlxfwreset`.
        reset_level: u8,
        /// Delay between post-reset verification attempts, in 1..=1800 seconds.
        /// Omission uses the supervisor's 10-second default; timing never
        /// requests a verification by itself.
        post_reset_verify_retry_interval_seconds: Option<u32>,
        /// Budget for all post-reset verification, in 1..=1800 seconds.
        /// Omission uses the supervisor's 1800-second default. This is not a
        /// deadline for host reconciliation.
        post_reset_verify_timeout_seconds: Option<u32>,
    },
    /// One host reboot after the last burn activates every card at once. Its
    /// requests set `reset: false` and leave `reset_level` at the supervisor's
    /// default, which Core does not store.
    HostReboot,
}

/// Lockdown policy read before changing the card; credentials are never saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicFirmwareLockdownPolicy {
    /// Leave a card that was already unlocked unlocked.
    Unlocked,
    /// Lock the card again under the exact key identity that locked it.
    Locked {
        /// Version of the site-wide lockdown input-key material.
        ikm_version: u32,
        /// MAC used with the owning host ID by the existing lockdown KDF.
        mac_address: MacAddress,
    },
}

/// A successful live lock status read; an unsupported or unknown status is not
/// unlocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicFirmwareLockdownStatus {
    /// The card is locked.
    Locked,
    /// The card is unlocked.
    Unlocked,
}

/// `NicFirmwareLockdownProgress` keeps a lockdown change separate from its readback.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NicFirmwareLockdownProgress {
    /// The lockdown command for the current phase.
    pub step: NicFirmwareLockdownStep,
    /// The report that answered `step`, or absence while delivery is pending.
    /// Only a report correlated with the request that asked belongs here, and
    /// `step` must not be resent while one is stored.
    pub result: Option<NicFirmwareLockdownResult>,
}

/// `NicFirmwareLockdownStep` identifies a status read or an idempotent lock change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicFirmwareLockdownStep {
    /// Read the selected card's lock status without changing it.
    #[default]
    Observe,
    /// Apply the lockdown policy required by the current phase.
    Change,
}

/// `NicFirmwareLockdownResult` is a card's answer to a lockdown command: a
/// status read, a completed change, or an error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicFirmwareLockdownResult {
    /// Lock status read from the exact selected card.
    Status(NicFirmwareLockdownStatus),
    /// The lock or unlock completed; a later status read is still required.
    Changed,
    /// The command failed or its effect is uncertain; read the status before
    /// retrying.
    Error {
        /// Nonsecret explanation retained until the next controller iteration.
        diagnostic: String,
    },
}

/// The controller's recovery point, separate from supervisor execution phases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NicFirmwarePhase {
    /// Read the card's receipt before letting this helper submit.
    ObservingSupervisor {
        /// Index of the card being processed.
        target_index: usize,
    },
    /// Get a live unlocked status before submitting firmware work.
    Unlocking {
        /// Index of the card being processed.
        target_index: usize,
    },
    /// Deliver or observe the immutable request until its execution settles.
    UpdatingFirmware {
        /// Index of the card being processed.
        target_index: usize,
    },
    /// Activate all completed burns through one planned host reboot.
    Rebooting {
        /// Database time when the activation stage was committed. Reboot reports
        /// must arrive after it and cover the latest attempt and power request;
        /// their receipt time does not identify a hardware boot.
        started_at: DateTime<Utc>,
        /// Database time of the last bounded reboot attempt, or absence before
        /// the first attempt.
        last_attempt_at: Option<DateTime<Utc>>,
    },
    /// Wait for fresh ordinary inventory that reports each card's exact
    /// requested version.
    Reobserving {
        /// Database time of the activation boundary, comparable with
        /// `MlxDeviceObservation::received_at`. Inventory received at or before
        /// it cannot establish success. Receipt time alone does not prove when
        /// Scout collected a report.
        not_before: DateTime<Utc>,
    },
    /// Restore and independently read back each card's original lockdown policy.
    RestoringLockdown {
        /// Index of the card whose lockdown is being restored.
        target_index: usize,
        /// Why firmware convergence failed, kept while lockdown is restored.
        /// Absence means every card converged.
        failure: Option<String>,
    },
    /// Lockdown restoration finished, but firmware convergence did not succeed.
    /// Like `RestoringLockdown`, this requires every card with a receipt to have
    /// settled safely (see `NicFirmwareTarget::can_restore_lockdown`).
    Failed {
        /// Nonsecret actionable explanation; this state does not permit a reburn.
        reason: String,
    },
}

impl NicFirmwareOperation {
    /// Return the card the current phase works on, or absence for host-wide phases.
    fn current_target_index(&self) -> Option<usize> {
        match self.phase {
            NicFirmwarePhase::ObservingSupervisor { target_index }
            | NicFirmwarePhase::Unlocking { target_index }
            | NicFirmwarePhase::UpdatingFirmware { target_index }
            | NicFirmwarePhase::RestoringLockdown { target_index, .. } => Some(target_index),
            NicFirmwarePhase::Rebooting { .. }
            | NicFirmwarePhase::Reobserving { .. }
            | NicFirmwarePhase::Failed { .. } => None,
        }
    }

    /// Return the card that may take an execution report. Only the current
    /// card qualifies, during initial observation or execution monitoring, and
    /// only when the report names this workflow as both its operation and
    /// phase, names the card's PCI address, and the card has no terminal
    /// receipt yet. Late progress therefore never replaces a settled result.
    fn reporting_target_mut(
        &mut self,
        operation_id: &str,
        phase_id: &str,
        pci_name: &str,
    ) -> Option<&mut NicFirmwareTarget> {
        let (NicFirmwarePhase::ObservingSupervisor { target_index }
        | NicFirmwarePhase::UpdatingFirmware { target_index }) = self.phase
        else {
            return None;
        };
        if operation_id != self.operation_id || phase_id != self.operation_id {
            return None;
        }
        self.targets.get_mut(target_index).filter(|target| {
            target.pci_name == pci_name
                && !target
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| is_terminal(receipt.state))
        })
    }

    /// Accept the current card's exact correlated receipt during initial
    /// observation or execution monitoring; a receipt never grants submission.
    /// Stale identifiers, repeated values and updates after a terminal receipt
    /// are ignored. The return value says whether the operation changed.
    pub fn accept_receipt(
        &mut self,
        operation_id: &str,
        phase_id: &str,
        pci_name: &str,
        receipt: FirmwareOperation,
    ) -> bool {
        let Some(target) = self.reporting_target_mut(operation_id, phase_id, pci_name) else {
            return false;
        };
        if receipt.operation_id != operation_id || target.receipt.as_ref() == Some(&receipt) {
            return false;
        }
        target.receipt = Some(receipt);
        target.diagnostic = None;
        true
    }

    /// Record the helper that answered a correlated read without a receipt.
    /// The first such answer during initial observation sets
    /// `supervisor_instance_id`. Later missing evidence records uncertainty
    /// without replacing that identity or a terminal receipt. Malformed
    /// identities and unrelated results are ignored. The return value says
    /// whether the operation changed.
    pub fn accept_missing_receipt(
        &mut self,
        operation_id: &str,
        phase_id: &str,
        pci_name: &str,
        supervisor_instance_id: String,
    ) -> bool {
        let observing = matches!(self.phase, NicFirmwarePhase::ObservingSupervisor { .. });
        if !valid_token(&supervisor_instance_id) {
            return false;
        }
        let Some(target) = self.reporting_target_mut(operation_id, phase_id, pci_name) else {
            return false;
        };
        if observing && target.supervisor_instance_id.is_none() && target.receipt.is_none() {
            target.supervisor_instance_id = Some(supervisor_instance_id);
            target.diagnostic = None;
            return true;
        }
        let diagnostic = if target.receipt.is_some() {
            "previously accepted firmware receipt is unavailable; observing without resubmitting"
        } else if target.supervisor_instance_id.as_ref() != Some(&supervisor_instance_id) {
            "firmware supervisor instance changed without a receipt; initial submission permission is not renewed"
        } else {
            return false;
        };
        if target.diagnostic.as_deref() == Some(diagnostic) {
            return false;
        }
        target.diagnostic = Some(diagnostic.to_string());
        true
    }

    /// Retain a correlated delivery failure without replacing settled execution
    /// evidence. This diagnostic never authorizes another helper instance.
    /// The return value says whether the operation changed.
    pub fn accept_error(
        &mut self,
        operation_id: &str,
        phase_id: &str,
        pci_name: &str,
        diagnostic: String,
    ) -> bool {
        // The wire carries a nonempty diagnostic of at most 2048 bytes; anything
        // else is malformed and must not grow the stored operation.
        if diagnostic.is_empty() || diagnostic.len() > 2048 {
            return false;
        }
        let Some(target) = self.reporting_target_mut(operation_id, phase_id, pci_name) else {
            return false;
        };
        if target.diagnostic.as_ref() == Some(&diagnostic) {
            return false;
        }
        target.diagnostic = Some(diagnostic);
        true
    }

    /// Validate operation identity, card uniqueness, request inputs and the
    /// evidence each persisted phase requires. This does not qualify hardware
    /// or authorize production admission.
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if !valid_token(&self.operation_id)
            || self.operation_id.starts_with("dpa-firmware:")
            || self.targets.is_empty()
        {
            return Err(ConfigValidationError::InvalidValue(
                "NIC firmware operation requires a valid identity and at least one target".into(),
            ));
        }
        if self
            .current_target_index()
            .is_some_and(|index| index >= self.targets.len())
        {
            return Err(ConfigValidationError::InvalidValue(
                "NIC firmware phase references a missing target".into(),
            ));
        }
        if let NicFirmwareActivation::CardReset {
            post_reset_verify_retry_interval_seconds,
            post_reset_verify_timeout_seconds,
            ..
        } = self.options.activation
            && [
                post_reset_verify_retry_interval_seconds,
                post_reset_verify_timeout_seconds,
            ]
            .into_iter()
            .flatten()
            .any(|seconds| !(1..=1800).contains(&seconds))
        {
            return Err(ConfigValidationError::InvalidValue(
                "NIC firmware post-reset verification timing must be in 1..=1800 seconds".into(),
            ));
        }
        for (index, target) in self.targets.iter().enumerate() {
            target.selector.clone().validate()?;
            if target
                .supervisor_instance_id
                .as_ref()
                .is_some_and(|token| !valid_token(token))
            {
                return Err(ConfigValidationError::InvalidValue(
                    "NIC firmware target has an invalid supervisor instance".into(),
                ));
            }
            // The shared parser owns numeric bounds; the byte check holds the
            // name to Scout's canonical spelling, which the parser would trim.
            let canonical_pci = matches!(
                target.pci_name.as_bytes(),
                [_, _, _, _, b':', _, _, b':', _, _, b'.', _]
            ) && target
                .pci_name
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f' | b':' | b'.'));
            if !canonical_pci || PciBdfOrderingKey::from_bdf(&target.pci_name).is_err() {
                return Err(ConfigValidationError::InvalidValue(
                    "NIC firmware target requires a full lowercase PCI BDF".into(),
                ));
            }
            let normalized = NicFirmwareProfileConfig {
                entries: vec![target.resolution.entry.clone()],
            }
            .validate_and_normalize()?;
            if normalized.entries.as_slice() != std::slice::from_ref(&target.resolution.entry) {
                return Err(ConfigValidationError::InvalidValue(
                    "NIC firmware target requires a normalized firmware entry".into(),
                ));
            }
            if let NicFirmwareLockdownPolicy::Locked { mac_address, .. } = target.original_lockdown
            {
                NicFirmwareCardSelector::Mac(mac_address).validate()?;
                if matches!(target.selector, NicFirmwareCardSelector::Mac(selected) if selected != mac_address)
                {
                    return Err(ConfigValidationError::InvalidValue(
                        "NIC firmware lockdown context does not match the selected card".into(),
                    ));
                }
            }
            if self.targets[..index].iter().any(|previous| {
                previous.selector == target.selector || previous.pci_name == target.pci_name
            }) {
                return Err(ConfigValidationError::InvalidValue(
                    "NIC firmware targets must have distinct identities and PCI locators".into(),
                ));
            }
        }
        // A persisted phase must carry the evidence its earlier phases gathered.
        // Cards run one at a time, so a card's work starts only after every
        // earlier card completed. Firmware goes out only under a frozen helper
        // instance, unless the card already has a receipt to observe.
        if let NicFirmwarePhase::ObservingSupervisor { target_index }
        | NicFirmwarePhase::Unlocking { target_index }
        | NicFirmwarePhase::UpdatingFirmware { target_index } = self.phase
        {
            if self.targets[..target_index]
                .iter()
                .any(|target| !target.is_complete(&self.operation_id, &self.options))
            {
                return Err(ConfigValidationError::InvalidValue(
                    "NIC firmware card work requires every earlier card to complete".into(),
                ));
            }
            let target = &self.targets[target_index];
            if !matches!(self.phase, NicFirmwarePhase::ObservingSupervisor { .. })
                && target.supervisor_instance_id.is_none()
                && target.receipt.is_none()
            {
                return Err(ConfigValidationError::InvalidValue(
                    "NIC firmware submission requires a frozen supervisor instance or a receipt"
                        .into(),
                ));
            }
        }
        if matches!(self.phase, NicFirmwarePhase::Rebooting { .. })
            && !matches!(self.options.activation, NicFirmwareActivation::HostReboot)
        {
            return Err(ConfigValidationError::InvalidValue(
                "NIC firmware host reboot requires host reboot activation".into(),
            ));
        }
        if matches!(
            self.phase,
            NicFirmwarePhase::Rebooting { .. }
                | NicFirmwarePhase::Reobserving { .. }
                | NicFirmwarePhase::RestoringLockdown { failure: None, .. }
        ) && self
            .targets
            .iter()
            .any(|target| !target.is_complete(&self.operation_id, &self.options))
        {
            return Err(ConfigValidationError::InvalidValue(
                "NIC firmware activation and success require evidence for every requested phase"
                    .into(),
            ));
        }
        if matches!(
            self.phase,
            NicFirmwarePhase::RestoringLockdown { .. } | NicFirmwarePhase::Failed { .. }
        ) && self.targets.iter().any(|target| {
            target.receipt.is_some() && !target.can_restore_lockdown(&self.operation_id)
        }) {
            return Err(ConfigValidationError::InvalidValue(
                "NIC firmware lockdown cannot be restored while execution effects remain uncertain"
                    .into(),
            ));
        }
        Ok(())
    }
}

impl NicFirmwareTarget {
    /// Whether this card's execution has settled safely enough to restore its
    /// lockdown policy: a completed receipt, or a failure or cancellation
    /// before configuration or burn with no reported mutation. A terminal label
    /// alone does not settle uncertain effects. A missing receipt returns
    /// false; callers may separately know that delivery never reached
    /// firmware submission.
    pub fn can_restore_lockdown(&self, operation_id: &str) -> bool {
        self.receipt.as_ref().is_some_and(|receipt| {
            receipt.operation_id == operation_id
                && match receipt.state {
                    FirmwareOperationState::Complete => true,
                    FirmwareOperationState::Failed | FirmwareOperationState::Canceled => {
                        matches!(
                            receipt.phase,
                            None | Some(
                                FirmwareOperationPhase::Inspect | FirmwareOperationPhase::Prepare
                            )
                        ) && !receipt.report.flashed
                            && receipt.report.reset != Some(true)
                            && receipt.report.verified_image != Some(true)
                            && receipt.report.verified_version != Some(true)
                    }
                    _ => false,
                }
        })
    }

    /// Whether this card's receipt proves a completed burn of its exact
    /// requested version, with successful evidence for exactly the phases
    /// `options` requested. Missing required evidence cannot be repaired by
    /// later inventory.
    pub fn is_complete(&self, operation_id: &str, options: &NicFirmwareExecutionOptions) -> bool {
        let version = self.resolution.entry.firmware.version.as_str();
        let reset = matches!(options.activation, NicFirmwareActivation::CardReset { .. });
        self.receipt.as_ref().is_some_and(|receipt| {
            receipt.operation_id == operation_id
                && receipt.is_complete()
                && receipt.report.flashed
                && receipt.report.expected_version.as_deref() == Some(version)
                && receipt.report.reset == reset.then_some(true)
                && receipt.report.verified_image == options.verify_image.then_some(true)
                && receipt.report.verified_version == options.verify_version.then_some(true)
                && (!options.verify_version
                    || receipt.report.observed_version.as_deref() == Some(version))
        })
    }
}

/// Matches the supervisor's token syntax: 1..=256 printable nonspace ASCII bytes.
fn valid_token(value: &str) -> bool {
    (1..=256).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Whether supervisor execution has stopped, so no later report can change the
/// receipt. `Interrupted` and `ManualIntervention` count as stopped: neither
/// permits completion or lockdown restoration, so the first one stored stands.
fn is_terminal(state: FirmwareOperationState) -> bool {
    match state {
        FirmwareOperationState::Accepted
        | FirmwareOperationState::Running
        | FirmwareOperationState::CancelRequested => false,
        FirmwareOperationState::Complete
        | FirmwareOperationState::Canceled
        | FirmwareOperationState::Interrupted
        | FirmwareOperationState::ManualIntervention
        | FirmwareOperationState::Failed => true,
    }
}
