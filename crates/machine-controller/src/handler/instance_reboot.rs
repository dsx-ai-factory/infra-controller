// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Ordinary instance reboot requests, executed by the existing machine controller.

use bmc_platform::ResetType;
use carbide_utils::redfish::BmcAccessInfo;
use db::TransactionVending as _;
use futures_util::FutureExt;
use model::hardware_dispatch::{HardwareDispatch, HardwareDispatchOwner};
use model::machine::{
    InstanceState, MachineLastRebootRequested, MachineLastRebootRequestedMode, ManagedHostState,
    ManagedHostStateSnapshot,
};
use state_controller::db_write_batch::WriteOpFn;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::MachineStateHandlerContextObjects;
use crate::redfish::PowerControlError;

/// Cancel an unsent reboot after provisioning fails or its allocation or BMC target changes.
pub(super) async fn cancel_unsent_request(
    snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
    let Some(dispatch) = pending_request(snapshot) else {
        return Ok(None);
    };
    let HardwareDispatchOwner::InstanceReboot {
        instance_id,
        last_attempt_at,
        bmc,
        ..
    } = &dispatch.owner
    else {
        return Ok(None);
    };
    if snapshot
        .instance
        .as_ref()
        .is_none_or(|instance| instance.id != *instance_id || instance.deleted.is_some())
        || bmc.ip != snapshot.host_snapshot.status.bmc_info.ip
        || bmc.port != snapshot.host_snapshot.status.bmc_info.port
        || bmc.mac != snapshot.host_snapshot.status.bmc_info.mac
        || matches!(
            snapshot.managed_state,
            ManagedHostState::Assigned {
                instance_state: InstanceState::Failed { .. },
            }
        )
    {
        if last_attempt_at.is_some() {
            // Release must still reach its existing cleanup states. Keep an uncertain
            // reboot recorded so it cannot silently allow a later NIC update.
            // Guarded recovery: https://github.com/dsx-ai-factory/infra-controller/issues/8942
            return Ok(None);
        }
        // Cancel stale unsent work even when the former BMC cannot be reached.
        // Failed provisioning cancels requests that have not been sent, even if BIOS
        // recovery can later return to Ready. A new request can follow that recovery.
        // Recheck under the normal lock order so a stale snapshot cannot cancel current work.
        let mut txn = ctx.services.db_pool.txn_begin().await?;
        let instance = db::instance::find_by_id_for_update(txn.as_mut(), *instance_id).await?;
        db::machine::lock_by_id_for_update(txn.as_mut(), &snapshot.host_snapshot.id.into()).await?;
        let current =
            db::machine::find_one(txn.as_mut(), &snapshot.host_snapshot.id, Default::default())
                .await?
                .ok_or_else(|| StateHandlerError::MissingData {
                    object_id: snapshot.host_snapshot.id.to_string(),
                    missing: "instance reboot host",
                })?;
        if instance.as_ref().is_none_or(|instance| {
            instance.machine_id != snapshot.host_snapshot.id || instance.deleted.is_some()
        }) || bmc.ip != current.status.bmc_info.ip
            || bmc.port != current.status.bmc_info.port
            || bmc.mac != current.status.bmc_info.mac
            || matches!(
                current.state.value,
                ManagedHostState::Assigned {
                    instance_state: InstanceState::Failed { .. },
                }
            )
        {
            settle(&mut txn, snapshot.host_snapshot.id, dispatch).await?;
            return Ok(Some(StateHandlerOutcome::do_nothing().with_txn(txn)));
        }
        return Ok(Some(
            StateHandlerOutcome::wait("instance reboot snapshot needs refreshing".into())
                .with_txn(txn),
        ));
    }
    Ok(None)
}

/// Find the queued reboot whose result still owns the host.
pub(super) fn pending_request(snapshot: &ManagedHostStateSnapshot) -> Option<&HardwareDispatch> {
    snapshot
        .host_snapshot
        .status
        .hardware_dispatch
        .as_ref()
        .filter(|dispatch| {
            dispatch.is_pending()
                && matches!(dispatch.owner, HardwareDispatchOwner::InstanceReboot { .. })
        })
}

/// Reconcile a queued reboot before automatic provisioning retries in Assigned/Ready.
pub(super) async fn handle(
    snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
    let Some(dispatch) = pending_request(snapshot) else {
        return Ok(None);
    };
    let HardwareDispatchOwner::InstanceReboot {
        last_attempt_at,
        reconcile_after,
        ..
    } = &dispatch.owner
    else {
        return Ok(None);
    };
    // Use the existing reboot retry interval with its one-minute minimum.
    // The interval permits another attempt; it never establishes completion.
    let retry_after = ctx
        .services
        .site_config
        .machine_state_controller
        .failure_retry_time
        .max(chrono::Duration::minutes(1));
    if let Some(attempt) = last_attempt_at {
        let now = db::machine::database_now(&ctx.services.db_pool).await?;
        let Some(reconcile_after) = reconcile_after else {
            return Err(StateHandlerError::ManualInterventionRequired(format!(
                "instance reboot {} predates saved worker reservations; reconcile its sender before clearing ownership",
                dispatch.token,
            )));
        };
        if now < *attempt + retry_after || now < *reconcile_after {
            return Ok(Some(StateHandlerOutcome::wait(
                "waiting before reconciling the instance reboot request".into(),
            )));
        }
    }
    // Preserve the original worker's reservation even when a replacement has a
    // shorter handling budget. The processor bounds the whole iteration; this
    // local deadline also bounds direct callers and preparation before checkpointing.
    let budget = ctx
        .services
        .site_config
        .machine_state_controller
        .controller
        .max_object_handling_time;
    let deadline = ctx.deadline.min(tokio::time::Instant::now() + budget);
    let reservation = chrono::Duration::from_std(budget.checked_mul(3).ok_or_else(|| {
        StateHandlerError::GenericError(eyre::eyre!("reboot handling budget is too large"))
    })?)
    .map_err(|error| StateHandlerError::GenericError(error.into()))?;
    before_deadline(
        deadline,
        attempt(snapshot, dispatch, retry_after, reservation, deadline, ctx),
    )
    .await
    .map(Some)
}

/// Stop an expired sender before polling a helper whose awaited response just became ready.
/// Tokio's timeout alone polls its inner future first; after a stalled runtime that
/// could otherwise resume an old power sequence after another worker took over.
async fn before_deadline<T>(
    deadline: tokio::time::Instant,
    work: impl Future<Output = Result<T, StateHandlerError>>,
) -> Result<T, StateHandlerError> {
    let mut work = std::pin::pin!(work);
    tokio::time::timeout_at(
        deadline,
        std::future::poll_fn(|cx| {
            if tokio::time::Instant::now() >= deadline {
                return std::task::Poll::Ready(Err(StateHandlerError::GenericError(eyre::eyre!(
                    "instance reboot attempt timed out"
                ))));
            }
            work.as_mut().poll(cx)
        }),
    )
    .await
    .map_err(|error| StateHandlerError::GenericError(error.into()))?
}

/// Read restart evidence independently, then prepare any resend and recheck the saved request.
async fn attempt(
    snapshot: &ManagedHostStateSnapshot,
    dispatch: &HardwareDispatch,
    retry_after: chrono::Duration,
    reservation: chrono::Duration,
    deadline: tokio::time::Instant,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let HardwareDispatchOwner::InstanceReboot {
        machine_id,
        instance_id,
        bmc,
        last_attempt_at,
        ..
    } = &dispatch.owner
    else {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "expected an instance reboot request"
        )));
    };
    if *machine_id != snapshot.host_snapshot.id {
        return Err(StateHandlerError::ManualInterventionRequired(
            "instance reboot request belongs to another host".into(),
        ));
    }
    if last_attempt_at.is_some()
        && (bmc.ip != snapshot.host_snapshot.status.bmc_info.ip
            || bmc.port != snapshot.host_snapshot.status.bmc_info.port
            || bmc.mac != snapshot.host_snapshot.status.bmc_info.mac
            || snapshot
                .instance
                .as_ref()
                .is_none_or(|instance| instance.id != *instance_id || instance.deleted.is_some()))
    {
        return Err(StateHandlerError::ManualInterventionRequired(format!(
            "instance reboot {} has an uncertain result and its target changed; reconcile the original host before clearing hardware ownership",
            dispatch.token,
        )));
    }
    // Prepare the captured IP, port and credential identity. A newer inventory
    // lookup must not select a different client that later passes the locked recheck.
    let access = BmcAccessInfo {
        host: bmc
            .ip
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: machine_id.to_string(),
                missing: "saved BMC IP",
            })?
            .to_string(),
        port: bmc.port,
        mac_address: bmc.mac.ok_or_else(|| StateHandlerError::MissingData {
            object_id: machine_id.to_string(),
            missing: "saved BMC MAC",
        })?,
    };
    // Restart evidence uses the existing read-only pool for both backends. An
    // unavailable mutation backend must not prevent settlement from valid evidence.
    let observed_restart = if let Some(attempt) = last_attempt_at {
        let observation_budget =
            deadline.saturating_duration_since(tokio::time::Instant::now()) / 2;
        let logs = tokio::time::timeout(observation_budget, async {
            let client = ctx
                .services
                .redfish_client_pool
                .client_by_info(&access)
                .await?;
            Ok::<_, eyre::Report>(client.get_bmc_event_log(Some(*attempt)).await?)
        })
        .await;
        match logs {
            Ok(Ok(logs)) => logs
                .iter()
                .any(|log| is_host_restart_after(&log.message, &log.created, *attempt)),
            Ok(Err(error)) => {
                tracing::debug!(host_machine_id = %machine_id, %error, "Instance reboot logs unavailable; retaining controller retry policy");
                false
            }
            Err(error) => {
                // A stalled read is not completion evidence. Leave time to evaluate
                // the operator hold and retry limit, or make a permitted attempt.
                tracing::debug!(host_machine_id = %machine_id, %error, "Instance reboot log read timed out; retaining controller retry policy");
                false
            }
        }
    } else {
        false
    };
    if !observed_restart
        && last_attempt_at.is_some()
        && let Some(outcome) = retry_outcome(
            dispatch,
            retry_after,
            db::machine::database_now(&ctx.services.db_pool).await?,
            snapshot
                .aggregate_health
                .is_reboot_blocked_in_state_machine(),
        )?
    {
        // A broken mutation client must not hide a hold or an exhausted retry window.
        // Observation stays first so either policy still permits confirmed settlement.
        return Ok(outcome);
    }
    let prepared = if observed_restart {
        None
    } else {
        Some(
            ctx.services
                .connect_bmc_for_mutation((*machine_id).into(), &access)
                .await?,
        )
    };

    // Preparation can wait on credentials/network. Recheck Instance then Machine under
    // the same locks used by the request API and NIC ownership; release them before I/O.
    let mut txn = ctx.services.db_pool.txn_begin().await?;
    let instance = db::instance::find_by_id_for_update(txn.as_mut(), *instance_id).await?;
    db::machine::lock_by_id_for_update(txn.as_mut(), &(*machine_id).into()).await?;
    db::machine_interface::lock_bmc_interfaces(txn.as_mut(), *machine_id).await?;
    let current = db::machine::find_one(txn.as_mut(), machine_id, Default::default())
        .await?
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: machine_id.to_string(),
            missing: "instance reboot host",
        })?;
    if current.status.hardware_dispatch.as_ref() != Some(dispatch)
        || current.state.version != snapshot.host_snapshot.state.version
        || current
            .reset_requested
            .as_ref()
            .is_some_and(|request| request.started_at.is_none())
    {
        return Ok(StateHandlerOutcome::wait(
            "instance reboot snapshot changed during preparation".into(),
        )
        .with_txn(txn));
    }
    let current_bmc = &current.status.bmc_info;
    let target_matches = instance
        .as_ref()
        .is_some_and(|instance| instance.machine_id == *machine_id && instance.deleted.is_none())
        && bmc.ip == current_bmc.ip
        && bmc.port == current_bmc.port
        && bmc.mac == current_bmc.mac;
    if !target_matches {
        if last_attempt_at.is_none() {
            // No sender has been authorized: cancel this stale request rather than redirect it.
            settle(&mut txn, *machine_id, dispatch).await?;
            tracing::warn!(host_machine_id = %machine_id, %instance_id, "Canceled unsent instance reboot because its target changed");
            return Ok(StateHandlerOutcome::do_nothing().with_txn(txn));
        }
        return Err(StateHandlerError::ManualInterventionRequired(format!(
            "instance reboot {} has an uncertain result and its target changed; reconcile the original host before clearing hardware ownership",
            dispatch.token,
        )));
    }
    if observed_restart {
        settle(&mut txn, *machine_id, dispatch).await?;
        let observed_at = last_attempt_at.unwrap_or(dispatch.started_at);
        // Another controller flow can have rebooted before this request is
        // reconciled. Keep that newer bookkeeping while settling the old owner.
        if current
            .status
            .last_reboot_requested
            .is_none_or(|request| request.time <= observed_at)
        {
            db::machine::record_reboot_request(
                &(*machine_id).into(),
                &mut txn,
                &MachineLastRebootRequested {
                    time: observed_at,
                    mode: MachineLastRebootRequestedMode::Reboot,
                    restart_verified: Some(true),
                    verification_attempts: Some(0),
                },
            )
            .await?;
        }
        return Ok(StateHandlerOutcome::do_nothing().with_txn(txn));
    }
    if last_attempt_at.is_some() {
        // Health changes do not advance the machine state version. Reload them after
        // client preparation so a newly committed operator hold stops this resend.
        let current_health = db::managed_host::load_by_machine_ids(
            &mut *txn,
            &[*machine_id],
            model::machine::LoadSnapshotOptions::default()
                .with_host_health(ctx.services.site_config.host_health),
        )
        .await?
        .remove(machine_id)
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: machine_id.to_string(),
            missing: "instance reboot health",
        })?;
        if let Some(outcome) = retry_outcome(
            dispatch,
            retry_after,
            db::machine::database_now(&mut *txn).await?,
            current_health
                .aggregate_health
                .is_reboot_blocked_in_state_machine(),
        )? {
            return Ok(outcome.with_txn(txn));
        }
    }
    if let Some(instance) = db::instance::find_by_id(&mut txn, *instance_id).await? {
        if instance.custom_pxe_reboot_requested {
            return Ok(StateHandlerOutcome::wait(
                "waiting for the custom PXE reboot to finish".into(),
            )
            .with_txn(txn));
        }
        if !instance
            .config
            .os
            .run_provisioning_instructions_on_every_boot
        {
            // The preceding custom-PXE flow may have restored one-time boot intent
            // after this ordinary request was saved. Execute the saved ordinary mode.
            db::instance::use_custom_ipxe_on_next_boot(machine_id, false, &mut txn).await?;
        }
    }
    if tokio::time::Instant::now() >= deadline {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "instance reboot preparation timed out"
        )));
    }
    let next = db::hardware_dispatch::record_instance_reboot_attempt(
        &mut txn,
        *machine_id,
        dispatch,
        reservation,
    )
    .await?
    .ok_or_else(|| {
        StateHandlerError::GenericError(eyre::eyre!("instance reboot ownership changed"))
    })?;
    txn.commit().await?;
    let HardwareDispatchOwner::InstanceReboot {
        last_attempt_at: Some(requested_at),
        ..
    } = next.owner
    else {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "expected an instance reboot attempt"
        )));
    };

    if tokio::time::Instant::now() >= deadline {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "instance reboot attempt timed out before dispatch"
        )));
    }
    // Uncertain errors and cancellation leave this attempt durable. A confirmed
    // non-send restores the earlier evidence boundary without releasing ownership.
    let sent = super::handler_host_power_control_with_bmc(
        snapshot,
        ctx,
        prepared.ok_or_else(|| {
            StateHandlerError::GenericError(eyre::eyre!("missing instance reboot mutation client"))
        })?,
        ResetType::ForceRestart,
        std::panic::Location::caller(),
        Some(requested_at),
        true,
    )
    .await;
    if matches!(&sent, Ok(false) | Err(PowerControlError::NotSent(_))) {
        // A non-send must not replace the timestamp used to recognize an earlier
        // uncertain effect. On the first attempt this restores an unsent request.
        let mut txn = ctx.services.db_pool.txn_begin().await?;
        db::hardware_dispatch::restore_instance_reboot_attempt(
            &mut txn,
            *machine_id,
            &next,
            dispatch,
        )
        .await?;
        txn.commit().await?;
    }
    match sent {
        Ok(true) => {}
        Ok(false) => {
            return Ok(StateHandlerOutcome::wait(
                "instance reboot was not sent because the host powered off".into(),
            ));
        }
        Err(error) => return Err(error.into_inner()),
    }
    let mut txn = ctx.services.db_pool.txn_begin().await?;
    settle(&mut txn, *machine_id, &next).await?;
    // Ordinary reboots finish on acceptance. Do not let the generic verifier send
    // another reboot when a tenant OS supplies no restart observation. Run after
    // the shared helper's tracking write. This conditional update only changes
    // that accepted record; coordinating the earlier write with other power writers
    // remains part of https://github.com/dsx-ai-factory/infra-controller/issues/8940.
    let machine_id = (*machine_id).into();
    let accepted = MachineLastRebootRequested {
        time: requested_at,
        mode: MachineLastRebootRequestedMode::Reboot,
        restart_verified: Some(false),
        verification_attempts: Some(0),
    };
    let write: WriteOpFn = Box::new(move |txn| {
        async move {
            let verification = db::machine::update_restart_verification_status(
                &machine_id,
                accepted,
                None,
                0,
                txn,
            )
            .await?;
            match verification {
                db::ConditionalWrite::Applied(()) | db::ConditionalWrite::NotApplied(_) => {
                    // A newer reboot record belongs to its own verifier. Keep it intact
                    // while settling this accepted request.
                    Ok(())
                }
            }
        }
        .boxed()
    });
    ctx.pending_db_writes.push(write);
    Ok(StateHandlerOutcome::do_nothing().with_txn(txn))
}

/// Apply the same retry decision before preparation and again under the authorization locks.
fn retry_outcome(
    dispatch: &HardwareDispatch,
    retry_after: chrono::Duration,
    now: chrono::DateTime<chrono::Utc>,
    reboot_blocked: bool,
) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
    if reboot_blocked {
        return Ok(Some(StateHandlerOutcome::wait(
            "automatic instance reboot retry is blocked by health override".into(),
        )));
    }
    // Match the controller's fifteen-interval recovery window. Expiry stops new
    // retries; it does not establish completion or release the saved request.
    if now >= dispatch.started_at + retry_after * 15 {
        return Err(StateHandlerError::ManualInterventionRequired(format!(
            "instance reboot {} exhausted its retry window; reconcile the unresolved request before clearing hardware ownership",
            dispatch.token,
        )));
    }
    Ok(None)
}

/// Recognize supported host restarts without treating generic power or DPU events as completion.
fn is_host_restart_after(
    message: &str,
    created: &str,
    attempt: chrono::DateTime<chrono::Utc>,
) -> bool {
    matches!(
        message,
        "Server reset." | "Server power restored." // HPE reset or the initial-Off On fallback
            | "The server is restarted by chassis control command." // Lenovo
            | "The property ResetType was assigned the value 'ForceWarmReboot' due to modification by the service." // GB200
    ) && chrono::DateTime::parse_from_rfc3339(created).is_ok_and(|created| created > attempt)
}

/// Clear exclusion only for the request whose response or cancellation was established.
async fn settle(
    txn: &mut sqlx::PgConnection,
    machine_id: carbide_uuid::machine::HostMachineId,
    dispatch: &HardwareDispatch,
) -> Result<(), StateHandlerError> {
    if !db::hardware_dispatch::settle(txn, machine_id, dispatch.token, &dispatch.owner).await? {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "instance reboot ownership changed before settlement"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Accept only supported host events strictly newer than the recorded attempt.
    #[test]
    fn restart_observation_requires_a_fresh_host_event() {
        let attempt = "2026-11-19T01:00:00.500Z".parse().unwrap();
        for (message, created, expected) in [
            // Each supported vendor/action must remain recognized.
            ("Server reset.", "2026-11-19T01:00:01Z", true),
            ("Server power restored.", "2026-11-19T01:00:01Z", true),
            (
                "The server is restarted by chassis control command.",
                "2026-11-19T01:00:01Z",
                true,
            ),
            (
                "The property ResetType was assigned the value 'ForceWarmReboot' due to modification by the service.",
                "2026-11-19T01:00:01Z",
                true,
            ),
            // Equality and a rounded same-second event cannot establish chronology.
            ("Server reset.", "2026-11-19T01:00:00.500Z", false),
            ("Server reset.", "2026-11-19T01:00:00Z", false),
            // Neither invalid timestamps nor unrelated power events prove this reboot.
            ("Server reset.", "invalid", false),
            (
                "Power supply redundancy lost",
                "2026-11-19T01:00:01Z",
                false,
            ),
        ] {
            assert_eq!(
                is_host_restart_after(message, created, attempt),
                expected,
                "{message}: {created}"
            );
        }
    }

    /// A ready response must not resume a suspended sender after its deadline.
    #[tokio::test(start_paused = true)]
    async fn expired_attempt_does_not_poll_a_ready_power_sequence() {
        let (release, ready) = tokio::sync::oneshot::channel();
        let dispatched = std::cell::Cell::new(false);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
        let work = before_deadline(deadline, async {
            ready.await.unwrap();
            dispatched.set(true);
            Ok(())
        });
        let mut work = std::pin::pin!(work);
        assert!(futures_util::poll!(work.as_mut()).is_pending());
        tokio::time::advance(std::time::Duration::from_secs(541)).await;
        release.send(()).unwrap();
        assert!(work.await.is_err());
        assert!(!dispatched.get());

        // The guard must still permit an ordinary ready operation within its budget.
        before_deadline(
            tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            async {
                dispatched.set(true);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert!(dispatched.get());
    }
}
