/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Reconcile an already admitted NIC firmware operation. Production admission is
//! tracked by https://github.com/dsx-ai-factory/infra-controller/issues/6680.
//! Each transition is a committed recovery point. Scout command delivery consumes
//! that state independently, while this handler owns activation and continuation.

use std::time::Duration;

use carbide_libmlx_model::firmware::operation::FirmwareOperationState;
use chrono::{DateTime, Utc};
use libredfish::SystemPowerControl;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::nic_firmware::{
    NicFirmwareActivation, NicFirmwareBaselineContinuation, NicFirmwareLockdownPolicy,
    NicFirmwareLockdownProgress, NicFirmwareLockdownResult, NicFirmwareLockdownStep,
    NicFirmwareOperation, NicFirmwarePhase, NicFirmwarePurpose, NicFirmwareTarget,
};
use model::machine::status::MlxDeviceObservation;
use model::machine::{
    CleanupState, InstanceState, MachineState, ManagedHostState, ManagedHostStateSnapshot,
};
use state_controller::CheckApplied;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::MachineStateHandlerContextObjects;

// One attempt includes client/credential lookup and the power request. A timeout
// leaves activation pending: only Scout's reboot report can advance the phase.
const REBOOT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(120);

pub(super) async fn handle(
    snapshot: &ManagedHostStateSnapshot,
    operation: &NicFirmwareOperation,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    operation.validate().map_err(|error| {
        StateHandlerError::ManualInterventionRequired(format!(
            "invalid admitted NIC firmware operation: {error}"
        ))
    })?;

    if snapshot
        .host_snapshot
        .status
        .hardware_dispatch
        .as_ref()
        .is_some_and(|dispatch| dispatch.is_pending())
    {
        return Ok(StateHandlerOutcome::wait(
            "an earlier hardware command is still awaiting its result".to_string(),
        ));
    }

    let machine_failure = super::get_failed_state(snapshot);
    let machine_failure_reason = machine_failure.as_ref().map(|(machine_id, details)| {
        // FailureDetails Display can include free text for some causes.
        // Keep the blocker bounded without changing that shared contract.
        format!(
            "NIC firmware is blocked by recorded machine failure on {machine_id}: {}/{}",
            details.source,
            details.cause.metric_label(),
        )
    });
    let stop_reason = machine_failure_reason
        .clone()
        .or_else(|| ownership_failure(snapshot, &operation.purpose));
    if let Some(reason) = stop_reason {
        let recovering = matches!(
            operation.phase,
            NicFirmwarePhase::Rebooting { .. }
                | NicFirmwarePhase::RestoringLockdown {
                    failure: Some(_),
                    ..
                }
                | NicFirmwarePhase::Failed { .. }
        );
        if !recovering {
            if let Some(target) = operation.current_target() {
                let possibly_submitted = target.receipt.is_some()
                    || matches!(operation.phase, NicFirmwarePhase::UpdatingFirmware { .. });
                if possibly_submitted && !target.can_restore_lockdown(&operation.operation_id) {
                    let reason = with_target_diagnostic(
                        target,
                        format!(
                            "{reason}; waiting for already authorized NIC firmware effects to settle"
                        ),
                    );
                    // An absent receipt in `UpdatingFirmware` is not proof that
                    // submission never happened, even on the same helper.
                    if target.receipt.is_none()
                        && target
                            .delivery_error_code
                            .is_some_and(|code| code.requires_intervention())
                    {
                        return Err(StateHandlerError::ManualInterventionRequired(reason));
                    }
                    if target
                        .receipt
                        .as_ref()
                        .is_none_or(|receipt| execution_pending(receipt.state))
                    {
                        return Ok(StateHandlerOutcome::wait(reason));
                    }
                    return Err(StateHandlerError::ManualInterventionRequired(reason));
                }
            }
            return Ok(transition(
                operation,
                NicFirmwarePhase::RestoringLockdown {
                    target_index: 0,
                    failure: Some(reason),
                },
            ));
        }
    }

    match &operation.phase {
        NicFirmwarePhase::ObservingSupervisor { target_index } => {
            let target = current_target(operation)?;
            if let Some(receipt) = &target.receipt {
                if !execution_pending(receipt.state)
                    && !target.can_restore_lockdown(&operation.operation_id)
                {
                    return Err(StateHandlerError::ManualInterventionRequired(
                        with_target_diagnostic(
                            target,
                            format!(
                                "NIC firmware operation {} has unresolved {:?} execution evidence",
                                operation.operation_id, receipt.state,
                            ),
                        ),
                    ));
                }
                // A retained receipt proves prior admission, not permission to
                // unlock or submit again. Resume status-only reconciliation.
                return Ok(transition(
                    operation,
                    NicFirmwarePhase::UpdatingFirmware {
                        target_index: *target_index,
                    },
                ));
            }
            if target
                .delivery_error_code
                .is_some_and(|code| code.requires_intervention())
            {
                return Err(StateHandlerError::ManualInterventionRequired(
                    target.diagnostic.clone().unwrap_or_else(|| {
                        "NIC firmware delivery requires operator intervention".into()
                    }),
                ));
            }
            if target.supervisor_instance_id.is_none() {
                return Ok(StateHandlerOutcome::wait(
                    target.diagnostic.clone().unwrap_or_else(|| {
                        "waiting for the initial NIC firmware supervisor observation".into()
                    }),
                ));
            }
            Ok(transition(
                operation,
                NicFirmwarePhase::Unlocking {
                    target_index: *target_index,
                },
            ))
        }
        NicFirmwarePhase::Unlocking { target_index } => {
            if let Some(pending) = reconcile_lockdown(operation)? {
                return Ok(pending);
            }
            Ok(transition(
                operation,
                NicFirmwarePhase::UpdatingFirmware {
                    target_index: *target_index,
                },
            ))
        }
        NicFirmwarePhase::UpdatingFirmware { target_index } => {
            let target = current_target(operation)?;
            let Some(receipt) = &target.receipt else {
                if target
                    .delivery_error_code
                    .is_some_and(|code| code.requires_intervention())
                {
                    return Err(StateHandlerError::ManualInterventionRequired(
                        target.diagnostic.clone().unwrap_or_else(|| {
                            "NIC firmware delivery requires operator intervention".into()
                        }),
                    ));
                }
                return Ok(StateHandlerOutcome::wait(
                    target.diagnostic.clone().unwrap_or_else(|| {
                        "waiting for the correlated NIC firmware receipt".into()
                    }),
                ));
            };
            if execution_pending(receipt.state) {
                return Ok(StateHandlerOutcome::wait(with_target_diagnostic(
                    target,
                    format!(
                        "NIC firmware operation {} is {:?}",
                        operation.operation_id, receipt.state
                    ),
                )));
            }
            if !target.is_complete(&operation.operation_id, &operation.options) {
                let reason = with_target_diagnostic(
                    target,
                    format!(
                        "NIC firmware operation {} ended in {:?} without successful requested-phase evidence",
                        operation.operation_id, receipt.state,
                    ),
                );
                if !target.can_restore_lockdown(&operation.operation_id) {
                    // Keep the receipt and execution phase visible. Terminal
                    // uncertainty does not permit another mutation or safe exit.
                    return Err(StateHandlerError::ManualInterventionRequired(reason));
                }
                return Ok(transition(
                    operation,
                    NicFirmwarePhase::RestoringLockdown {
                        target_index: 0,
                        failure: Some(reason),
                    },
                ));
            }
            if *target_index + 1 < operation.targets.len() {
                return Ok(transition(
                    operation,
                    NicFirmwarePhase::ObservingSupervisor {
                        target_index: *target_index + 1,
                    },
                ));
            }
            let now = db::machine::database_now(&ctx.services.db_pool).await?;
            let phase = if matches!(
                operation.options.activation,
                NicFirmwareActivation::HostReboot
            ) {
                // All burn receipts and this activation intent commit before the
                // first reboot. A lost runtime journal cannot recreate the burns.
                NicFirmwarePhase::Rebooting {
                    started_at: now,
                    last_attempt_at: None,
                }
            } else {
                NicFirmwarePhase::Reobserving { not_before: now }
            };
            Ok(transition(operation, phase))
        }
        NicFirmwarePhase::Rebooting {
            started_at,
            last_attempt_at,
        } => {
            // Recheck feedback under the state and machine locks before either
            // confirming activation or committing another attempt. A snapshot
            // taken before Scout's report must not cause an avoidable reboot.
            let version = snapshot.host_snapshot.state.version;
            let host_machine_id = snapshot.host_snapshot.id;
            let mut txn = ctx.services.db_pool.begin().await?;
            db::machine::lock_nic_firmware_state(&mut txn, &host_machine_id, version)
                .await?
                .check_applied()?;
            let current = db::machine::find_one(
                txn.as_mut(),
                &host_machine_id,
                MachineSearchConfig::default(),
            )
            .await?
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: host_machine_id.to_string(),
                missing: "NIC firmware host after state lock",
            })?;
            if let Some(reported_at) = current.status.last_reboot_time
                && reboot_report_covers_attempt(
                    reported_at,
                    *started_at,
                    *last_attempt_at,
                    current
                        .status
                        .last_reboot_requested
                        .map(|request| request.time),
                )
            {
                // `RebootCompleted` is Core receipt ordering, not a hardware
                // boot ID. Inventory still has to arrive after this boundary.
                if let Some(request) = current.status.last_reboot_requested
                    && request.restart_verified == Some(false)
                {
                    db::machine::update_restart_verification_status(
                        &host_machine_id.into(),
                        request,
                        Some(true),
                        0,
                        &mut txn,
                    )
                    .await?
                    .check_applied()?;
                }
                return Ok(transition(
                    operation,
                    NicFirmwarePhase::Reobserving {
                        not_before: reported_at,
                    },
                )
                .with_txn(txn));
            }
            if snapshot
                .aggregate_health
                .is_reboot_blocked_in_state_machine()
            {
                return Ok(StateHandlerOutcome::wait(
                    "NIC firmware activation reboot is blocked by a health override".into(),
                )
                .with_txn(txn));
            }
            // Count from the persisted attempt start. The minimum includes the
            // bounded 120-second call plus a minute for Scout to report back.
            let retry_delay = ctx
                .services
                .site_config
                .machine_state_controller
                .failure_retry_time
                .max(chrono::Duration::seconds(
                    REBOOT_ATTEMPT_TIMEOUT.as_secs() as i64 + 60,
                ));
            let now = db::machine::database_now(&mut *txn).await?;
            if last_attempt_at.is_some_and(|attempt| now < attempt + retry_delay) {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for Scout to report the NIC firmware activation reboot".into(),
                )
                .with_txn(txn));
            }
            // Commit the attempt marker before external work so retries retain
            // the same interval across controller crashes. A crash before sending
            // only delays the next attempt by that interval.
            // This checkpoint updates progress within Rebooting through state
            // persistence and history. Phase changes use the processor's hooks.
            let mut next = operation.clone();
            next.phase = NicFirmwarePhase::Rebooting {
                started_at: *started_at,
                last_attempt_at: Some(now),
            };
            db::machine::try_update_controller_state(
                &mut txn,
                &host_machine_id,
                version,
                version.increment(),
                &ManagedHostState::NicFirmwareUpdating { operation: next },
            )
            .await?
            .check_applied()?;
            txn.commit().await?;

            let attempt = tokio::time::timeout(
                REBOOT_ATTEMPT_TIMEOUT,
                super::handler_host_power_control_with_location(
                    snapshot,
                    ctx,
                    SystemPowerControl::ForceRestart,
                    std::panic::Location::caller(),
                    Some(now),
                ),
            )
            .await;
            match attempt {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(
                    host_machine_id = %snapshot.host_snapshot.id,
                    error = %error,
                    "NIC firmware activation reboot attempt failed"
                ),
                Err(error) => tracing::warn!(
                    host_machine_id = %snapshot.host_snapshot.id,
                    error = %error,
                    "NIC firmware activation reboot attempt timed out"
                ),
            }
            // Even an accepted power request is not reboot-completion evidence.
            // The attempt state already committed; do not repeat its CAS in the
            // outer processor. Scout feedback will enqueue the next iteration.
            Ok(StateHandlerOutcome::wait(
                "waiting for Scout to report the NIC firmware activation reboot".into(),
            ))
        }
        NicFirmwarePhase::Reobserving { not_before } => {
            let Some(observation) = &snapshot.host_snapshot.status.mlx_device_observation else {
                return Ok(StateHandlerOutcome::wait(
                    "waiting for ordinary NIC inventory after activation".into(),
                ));
            };
            if let Err(reason) = accept_inventory(operation, observation, *not_before) {
                return Ok(StateHandlerOutcome::wait(reason));
            }
            let mut next = operation.clone();
            next.phase = NicFirmwarePhase::RestoringLockdown {
                target_index: 0,
                failure: None,
            };
            next.lockdown = NicFirmwareLockdownProgress::default();
            Ok(StateHandlerOutcome::transition(
                ManagedHostState::NicFirmwareUpdating { operation: next },
            ))
        }
        NicFirmwarePhase::RestoringLockdown {
            target_index,
            failure,
        } => {
            if let Some(pending) = reconcile_lockdown(operation)? {
                return Ok(pending);
            }
            if *target_index + 1 < operation.targets.len() {
                return Ok(transition(
                    operation,
                    NicFirmwarePhase::RestoringLockdown {
                        target_index: *target_index + 1,
                        failure: failure.clone(),
                    },
                ));
            }
            match failure {
                None => complete(snapshot, operation, ctx).await,
                Some(reason) => Ok(transition(
                    operation,
                    NicFirmwarePhase::Failed {
                        reason: reason.clone(),
                    },
                )),
            }
        }
        NicFirmwarePhase::Failed { reason } => {
            if machine_failure.is_none()
                && let NicFirmwarePurpose::Allocation { instance_id, .. } = operation.purpose
                && snapshot.instance.as_ref().is_some_and(|instance| {
                    instance.id == instance_id && instance.deleted.is_some()
                })
            {
                // Only safe, independently restored failure reaches this phase.
                // A deleted original owner must still follow normal network and
                // instance teardown; jumping to cleanup would skip those steps.
                let mut txn = ctx.services.db_pool.begin().await?;
                db::machine::prepare_nic_firmware_owner_release(
                    &mut txn,
                    &snapshot.host_snapshot.id,
                    snapshot.host_snapshot.state.version,
                    &instance_id,
                )
                .await?
                .check_applied()?;
                return Ok(StateHandlerOutcome::transition(ManagedHostState::Assigned {
                    instance_state: InstanceState::WaitingForRebootToReady,
                })
                .with_txn(txn));
            }
            let reason = match machine_failure_reason {
                Some(blocker) if !reason.contains(blocker.as_str()) => {
                    format!("{reason}; {blocker}")
                }
                _ => reason.clone(),
            };
            Err(StateHandlerError::ManualInterventionRequired(reason))
        }
    }
}

/// Require a target for a card-specific phase before any command is considered.
fn current_target(
    operation: &NicFirmwareOperation,
) -> Result<&NicFirmwareTarget, StateHandlerError> {
    operation.current_target().ok_or_else(|| {
        StateHandlerError::ManualInterventionRequired(
            "NIC firmware phase has no valid frozen target".into(),
        )
    })
}

/// Check the lifecycle owner before effects; completion repeats this check
/// transactionally so a snapshot race cannot resume a stale caller.
fn ownership_failure(
    snapshot: &ManagedHostStateSnapshot,
    purpose: &NicFirmwarePurpose,
) -> Option<String> {
    let owner = snapshot
        .instance
        .as_ref()
        .map(|instance| (instance.id, instance.deleted.is_some()));
    (!purpose.matches_owner(owner))
        .then(|| "NIC firmware lifecycle owner changed; no new cards will be submitted".into())
}

/// Start each phase with fresh lockdown readback, retaining all frozen requests.
fn transition(
    operation: &NicFirmwareOperation,
    phase: NicFirmwarePhase,
) -> StateHandlerOutcome<ManagedHostState> {
    let mut next = operation.clone();
    if let NicFirmwarePhase::Unlocking { target_index }
    | NicFirmwarePhase::RestoringLockdown { target_index, .. } = operation.phase
        && let Some(NicFirmwareLockdownResult::Status(status)) = &operation.lockdown.result
        && Some(*status) == operation.desired_lockdown()
        && let Some(target) = next.targets.get_mut(target_index)
    {
        target.diagnostic = None;
    }
    next.phase = phase;
    next.lockdown = NicFirmwareLockdownProgress::default();
    StateHandlerOutcome::transition(ManagedHostState::NicFirmwareUpdating { operation: next })
}

/// Consume one lockdown report before authorizing the next command. Even a change
/// error returns to observation: the command might have changed the card before
/// its response was lost, so its reply cannot establish the final policy.
fn reconcile_lockdown(
    operation: &NicFirmwareOperation,
) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
    let desired = operation.desired_lockdown().ok_or_else(|| {
        StateHandlerError::ManualInterventionRequired(
            "NIC firmware lockdown phase has no valid frozen target".into(),
        )
    })?;
    let target = current_target(operation)?;
    let change_required = operation.lockdown.step == NicFirmwareLockdownStep::Change
        || (operation.lockdown.step == NicFirmwareLockdownStep::Observe
            && matches!(
                &operation.lockdown.result,
                Some(NicFirmwareLockdownResult::Status(status)) if *status != desired
            ));
    if change_required
        && matches!(
            target.original_lockdown,
            NicFirmwareLockdownPolicy::Unlocked
        )
    {
        return Err(StateHandlerError::ManualInterventionRequired(
            with_target_diagnostic(
                target,
                "NIC firmware lockdown change requires a frozen key identity".into(),
            ),
        ));
    }
    let Some(result) = &operation.lockdown.result else {
        return Ok(Some(StateHandlerOutcome::wait(with_target_diagnostic(
            target,
            "waiting for the correlated NIC lockdown report".into(),
        ))));
    };
    let (step, diagnostic) = match (operation.lockdown.step, result) {
        (NicFirmwareLockdownStep::Observe, NicFirmwareLockdownResult::Status(status))
            if *status == desired =>
        {
            return Ok(None);
        }
        (NicFirmwareLockdownStep::Observe, NicFirmwareLockdownResult::Status(_)) => {
            (NicFirmwareLockdownStep::Change, None)
        }
        (NicFirmwareLockdownStep::Change, NicFirmwareLockdownResult::Changed) => {
            (NicFirmwareLockdownStep::Observe, None)
        }
        (_, NicFirmwareLockdownResult::Error { diagnostic }) => {
            (NicFirmwareLockdownStep::Observe, Some(diagnostic.clone()))
        }
        _ => {
            return Err(StateHandlerError::ManualInterventionRequired(
                "NIC firmware lockdown report does not match its requested step".into(),
            ));
        }
    };
    let mut next = operation.clone();
    next.lockdown = NicFirmwareLockdownProgress { step, result: None };
    let target_index = match operation.phase {
        NicFirmwarePhase::Unlocking { target_index }
        | NicFirmwarePhase::RestoringLockdown { target_index, .. } => target_index,
        _ => {
            return Err(StateHandlerError::ManualInterventionRequired(
                "NIC firmware lockdown report has no current lockdown phase".into(),
            ));
        }
    };
    let target = next.targets.get_mut(target_index).ok_or_else(|| {
        StateHandlerError::ManualInterventionRequired(
            "NIC firmware lockdown report has no current target".into(),
        )
    })?;
    target.diagnostic = diagnostic;
    Ok(Some(StateHandlerOutcome::transition(
        ManagedHostState::NicFirmwareUpdating { operation: next },
    )))
}

/// Pending execution can still change the card; keep observing its receipt.
fn execution_pending(state: FirmwareOperationState) -> bool {
    matches!(
        state,
        FirmwareOperationState::Accepted
            | FirmwareOperationState::Running
            | FirmwareOperationState::CancelRequested
    )
}

/// Include retained delivery evidence when explaining why a phase is blocked.
fn with_target_diagnostic(target: &NicFirmwareTarget, reason: String) -> String {
    match target.diagnostic.as_deref().or_else(|| {
        target
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.diagnostic.as_deref())
    }) {
        Some(diagnostic) => format!("{reason}: {diagnostic}"),
        None => reason,
    }
}

/// A reboot report must follow activation and cover the latest durable attempt.
fn reboot_report_covers_attempt(
    reported_at: DateTime<Utc>,
    started_at: DateTime<Utc>,
    last_attempt_at: Option<DateTime<Utc>>,
    requested_at: Option<DateTime<Utc>>,
) -> bool {
    reported_at > started_at
        && last_attempt_at.is_none_or(|attempt| reported_at >= attempt)
        && requested_at.is_none_or(|request| reported_at >= request)
}

/// Accept the whole frozen target set from one post-activation inventory. A
/// partial, stale, ambiguous or mismatched report remains retryable. Evidence is
/// accepted only by the controller transition after this whole-set check.
fn accept_inventory(
    operation: &NicFirmwareOperation,
    observation: &MlxDeviceObservation,
    not_before: DateTime<Utc>,
) -> Result<(), String> {
    let received_at = observation
        .received_at
        .ok_or_else(|| "waiting for inventory with a Core receipt timestamp".to_string())?;
    // Receipt time alone cannot distinguish a resend of an old collection.
    // These comparisons require synchronized Scout and database clocks.
    if received_at <= not_before || observation.observed_at <= not_before {
        return Err("waiting for inventory collected after firmware activation".into());
    }
    for target in &operation.targets {
        let index = target
            .selector
            .device_index(&observation.devices)
            .map_err(|error| error.to_string())?;
        let device = &observation.devices[index];
        let firmware = &target.resolution.entry.firmware;
        if device.pci_name != target.pci_name
            || device.part_number.as_deref() != Some(firmware.part_number.as_str())
            || device.psid.as_deref() != Some(firmware.psid.as_str())
            || device.fw_version_current.as_deref() != Some(firmware.version.as_str())
            || device.fw_version_running.as_deref() != Some(firmware.version.as_str())
        {
            return Err(format!(
                "waiting for the requested firmware identity and version on {}",
                target.pci_name
            ));
        }
    }
    Ok(())
}

/// Resume the frozen lifecycle purpose only after all cards have converged and
/// their original lockdown policies have been independently observed.
async fn complete(
    snapshot: &ManagedHostStateSnapshot,
    operation: &NicFirmwareOperation,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let continuation = match &operation.purpose {
        NicFirmwarePurpose::Baseline { continuation } => match continuation {
            NicFirmwareBaselineContinuation::InitialPreparation { skip_reboot_wait } => {
                ManagedHostState::HostInit {
                    machine_state: MachineState::Discovered {
                        skip_reboot_wait: *skip_reboot_wait,
                    },
                }
            }
            NicFirmwareBaselineContinuation::Ready => ManagedHostState::Ready,
        },
        NicFirmwarePurpose::Allocation { .. } => ManagedHostState::Assigned {
            instance_state: InstanceState::DpaProvisioning,
        },
        NicFirmwarePurpose::RestoreBaseline {
            cleanup_context, ..
        } => ManagedHostState::WaitingForCleanup {
            cleanup_state: CleanupState::Init,
            cleanup_context: *cleanup_context,
        },
    };
    // Recheck the owner under the same locks and transaction as the ordinary
    // controller state transition, so a stale snapshot cannot hand off a host.
    let mut txn = ctx.services.db_pool.begin().await?;
    db::machine::prepare_nic_firmware_completion(
        &mut txn,
        &snapshot.host_snapshot.id,
        snapshot.host_snapshot.state.version,
        &operation.purpose,
    )
    .await?
    .check_applied()?;
    Ok(StateHandlerOutcome::transition(continuation).with_txn(txn))
}

#[cfg(test)]
mod tests {
    use carbide_libmlx_model::device::info::MlxDeviceInfo;
    use carbide_test_support::{Case, Check, Outcome, check_cases, check_values};
    use model::nic_firmware::NicFirmwareCardSelector;
    use model::test_support::nic_firmware::operation;

    use super::*;

    /// Ordinary inventory for the fixture card, with matching installed and
    /// running firmware. Cases alter only the fact whose acceptance they test.
    fn inventory(barrier: DateTime<Utc>) -> MlxDeviceObservation {
        MlxDeviceObservation {
            observed_at: barrier + chrono::Duration::seconds(1),
            received_at: Some(barrier + chrono::Duration::seconds(1)),
            devices: vec![MlxDeviceInfo {
                pci_name: "0000:01:00.0".into(),
                device_type: "test-card".into(),
                device_description: None,
                base_mac: Some("02:00:00:00:00:01".parse().expect("fixture MAC")),
                base_guid: None,
                part_number: Some("test-card".into()),
                psid: Some("test-psid".into()),
                fw_version_current: Some("1.2.3".into()),
                fw_version_running: Some("1.2.3".into()),
                pxe_version_current: None,
                uefi_version_current: None,
                uefi_version_virtio_blk_current: None,
                uefi_version_virtio_net_current: None,
                status: None,
            }],
        }
    }

    /// Collection and receipt ordering protect different boundaries: resending
    /// old data is not activation proof, even with a new database receipt time.
    #[test]
    fn inventory_must_be_fresh_and_match_frozen_running_firmware() {
        let barrier = "2026-10-27T12:00:00Z".parse().expect("fixture time");
        let report = |edit: fn(&mut MlxDeviceObservation)| {
            let mut report = inventory(barrier);
            edit(&mut report);
            report
        };
        check_cases(
            [
                Case {
                    scenario: "fresh installed and running versions confirm activation",
                    input: report(|_| {}),
                    expect: Outcome::Yields(()),
                },
                Case {
                    scenario: "new receipt cannot freshen a preactivation collection",
                    input: report(|report| report.observed_at -= chrono::Duration::seconds(2)),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "a collection at the activation boundary is not later evidence",
                    input: report(|report| report.observed_at -= chrono::Duration::seconds(1)),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "a receipt at the activation boundary is not later evidence",
                    input: report(|report| {
                        report.received_at = Some(report.observed_at - chrono::Duration::seconds(1))
                    }),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "legacy inventory has no Core reception boundary",
                    input: report(|report| report.received_at = None),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "selector reuse at a different PCI function is not the frozen card",
                    input: report(|report| report.devices[0].pci_name = "0000:02:00.0".into()),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "missing part number cannot establish hardware compatibility",
                    input: report(|report| report.devices[0].part_number = None),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "a different PSID is not the selected firmware target",
                    input: report(|report| report.devices[0].psid = Some("different-psid".into())),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "running target alone does not replace installed-image evidence",
                    input: report(|report| {
                        report.devices[0].fw_version_current = Some("old-version".into())
                    }),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "installed target alone does not prove it is running",
                    input: report(|report| {
                        report.devices[0].fw_version_running = Some("old-version".into())
                    }),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "an unavailable running fact cannot certify activation",
                    input: report(|report| report.devices[0].fw_version_running = None),
                    expect: Outcome::Fails,
                },
                Case {
                    scenario: "an ambiguous physical selector cannot choose a card",
                    input: report(|report| report.devices.push(report.devices[0].clone())),
                    expect: Outcome::Fails,
                },
            ],
            |report| accept_inventory(&operation(), &report, barrier),
        );
    }

    /// One matching card cannot advance a host while a later frozen card still
    /// runs its old firmware. A later complete report may arrive in any order.
    #[test]
    fn inventory_confirms_the_whole_frozen_target_set() {
        let barrier = "2026-10-27T12:00:00Z".parse().expect("fixture time");
        let mut operation = operation();
        let mut report = inventory(barrier);
        let second_mac = "02:00:00:00:00:02".parse().expect("fixture MAC");
        let second_device = MlxDeviceInfo {
            pci_name: "0000:02:00.0".into(),
            base_mac: Some(second_mac),
            ..report.devices[0].clone()
        };
        let mut second_target = operation.targets[0].clone();
        second_target.selector = NicFirmwareCardSelector::Mac(second_mac);
        second_target.pci_name = second_device.pci_name.clone();
        operation.targets.push(second_target);
        report.devices.push(MlxDeviceInfo {
            fw_version_running: Some("old-version".into()),
            ..second_device.clone()
        });
        assert!(accept_inventory(&operation, &report, barrier).is_err());

        // Inventory order does not select the target: the physical selector does.
        report.devices = vec![second_device, report.devices.remove(0)];
        accept_inventory(&operation, &report, barrier).expect("complete later inventory");
    }

    /// A delayed report from an earlier retry must not verify a newer request.
    /// These timestamps are Core receipt ordering, not hardware boot identity.
    #[test]
    fn reboot_report_must_cover_activation_latest_attempt_and_latest_request() {
        let started_at = "2026-10-27T12:00:00Z".parse().expect("fixture time");
        let earlier = started_at - chrono::Duration::seconds(1);
        let later = started_at + chrono::Duration::seconds(1);
        check_values(
            [
                Check {
                    scenario: "report at activation boundary is stale",
                    input: (started_at, None, None),
                    expect: false,
                },
                Check {
                    scenario: "report before latest committed attempt",
                    input: (later, Some(later + chrono::Duration::seconds(1)), None),
                    expect: false,
                },
                Check {
                    scenario: "report before newer power request",
                    input: (
                        later,
                        Some(started_at),
                        Some(later + chrono::Duration::seconds(1)),
                    ),
                    expect: false,
                },
                Check {
                    scenario: "report covers every persisted boundary",
                    input: (later, Some(started_at), Some(started_at)),
                    expect: true,
                },
                Check {
                    scenario: "report after activation covers an older request",
                    input: (later, None, Some(earlier)),
                    expect: true,
                },
            ],
            |(reported_at, last_attempt_at, requested_at)| {
                reboot_report_covers_attempt(reported_at, started_at, last_attempt_at, requested_at)
            },
        );
    }
}
