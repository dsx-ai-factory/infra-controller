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

//! Machine-controller SuperNIC lockdown IKM rekey (`RotatingNicLockdown`).
//!
//! This is the idle-card completion path for NIC lockdown rotation, the direct
//! analog of the shipped host-UEFI Ready-entered rotation ([`super::host_uefi_rotation`]).
//! Lazy migration on the tenant cycle never completes for cards on idle `Ready`
//! hosts, because tenant release does **not** unlock a card -- it sits locked
//! under the old IKM indefinitely. This state drives every lagging SuperNIC
//! (SVPC) card on an otherwise-idle host through a tenant-free
//! `RotateKeyUnlocking -> RotateKeyLocking` cycle (unlock at the IKM the card is
//! locked under, relock at the site-wide target), waiting until each card
//! converges or is quarantined.
//!
//! Unlike the host/DPU credentials, the actual key change is performed by the
//! DPA interface state machine + scout, not by a Redfish/BIOS job. So this
//! module is a thin driver: it decides *whether* to enter
//! ([`should_enter_nic_lockdown_rotation`]) and, once in the state, kicks idle
//! cards into the rekey cycle, bounds unreachable cards with a per-step timeout +
//! backoff quarantine so the host never wedges, and returns to `Ready` once every
//! card has converged or been attempted ([`handle_rotating_nic_lockdown`]).
//!
//! Convergence/quarantine is recorded against the same `device_credential_rotation`
//! bookkeeping the other families use, keyed by each card's **NIC MAC**.

use db::ConditionalWrite::{Applied, NotApplied};
use db::ControllerStateNotCurrent;
use eyre::eyre;
use model::dpa_interface::{DpaInterface, DpaInterfaceControllerState, DpaInterfaceType};
use model::machine::{ManagedHostState, ManagedHostStateSnapshot};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::{MachineStateHandlerContextObjects, MachineStateHandlerServices};

/// How long a single rekey step (`RotateKeyUnlocking` or `RotateKeyLocking`) may
/// sit without the card reporting the expected lock mode before we treat the card
/// as unreachable, quarantine it with backoff, and reset it to `Ready`. Each step
/// gets its own budget (the version timestamp resets on the unlock->relock
/// transition), so the worst-case per-card cost is two budgets, kept comfortably
/// under the `ROTATING_NIC_LOCKDOWN` host SLA.
const REKEY_STEP_TIMEOUT_MINUTES: i64 = 15;

/// Whether an otherwise-idle `Ready` host should enter
/// `ManagedHostState::RotatingNicLockdown` now.
///
/// An operator force-converge request always wins -- the ops escape hatch is
/// honored even when the site-wide flag is off, and it later bypasses each card's
/// backoff quarantine. Otherwise the passive gate fires only when NIC lockdown
/// rekey is enabled site-wide *and* some SVPC card on the host lags the staged
/// target; the cheap flag is checked first so a disabled site never runs the gate
/// query. The idle-only constraint is enforced by the caller: this guard is the
/// lowest-precedence check in the `Ready` handler, reached only when the host has
/// no instance and no higher-priority lifecycle work, so a forced rekey of a busy
/// host is deferred to its next idle window rather than unlocking under tenancy.
pub(crate) async fn should_enter_nic_lockdown_rotation(
    services: &MachineStateHandlerServices,
    mh: &ManagedHostStateSnapshot,
) -> Result<bool, StateHandlerError> {
    // An operator force-converge request drives entry on its own.
    if mh.host_snapshot.lockdown_ikm_credential_rotation_requested {
        return Ok(true);
    }
    // Cheap site-wide flag first, so a disabled site never runs the gate query.
    if !services.site_config.nic_lockdown_ikm_rotation_enabled {
        return Ok(false);
    }
    // The gate reports `true` for a NIC MAC whose lockdown_ikm credential lags the
    // staged site-wide target and is not quarantined. A host can carry several
    // SVPC cards, so any lagging card is enough to enter; the aggregate check
    // inside the gate is cached, so the extra per-card lookups are cheap.
    for mac in svpc_interfaces(mh).map(|iface| iface.mac_address) {
        if services
            .nic_lockdown_rotation_gate
            .rotation_needed(&services.db_pool, mac)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("nic lockdown rotation gate query: {e}"))
            })?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The host's SuperNIC (SVPC) DPA interfaces. Shared by the entry guard and the
/// state body so the SVPC predicate lives in one place; each card's NIC MAC keys
/// its `lockdown_ikm` rotation bookkeeping.
fn svpc_interfaces(mh: &ManagedHostStateSnapshot) -> impl Iterator<Item = &DpaInterface> + '_ {
    mh.dpa_interface_snapshots
        .iter()
        .filter(|iface| iface.interface_type == DpaInterfaceType::Svpc)
}

/// Drive the host's SuperNIC cards through a tenant-free rekey to the site-wide
/// `lockdown_ikm` target, then return to `Ready`.
///
/// Each tick derives progress from the durable `device_credential_rotation`
/// bookkeeping (keyed by NIC MAC) and each card's DPA controller state:
///
/// - A tracked card that lags the target and sits idle (`Ready`) is kicked into
///   `RotateKeyUnlocking`; the DPA state machine + scout then unlock it at its
///   current IKM and relock it at the target, promoting `current_version` on the
///   observed relock.
/// - A card already mid-rekey is waited on, but bounded: if a single step exceeds
///   [`REKEY_STEP_TIMEOUT_MINUTES`] the card is deemed unreachable, quarantined
///   with backoff, and reset to `Ready` so the host never wedges here.
/// - A quarantined card is skipped (it does not block completion) unless an
///   operator force request is set, which re-attempts it.
///
/// The host settles back to `Ready` once every tracked SVPC card has converged or
/// been attempted (quarantined) this cycle; a one-shot force request is cleared
/// on that settled transition. Untracked cards (no bookkeeping row -- never
/// locked) are not rekey candidates and are ignored.
pub(crate) async fn handle_rotating_nic_lockdown(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    use db::credential_rotation::CredentialRotationType::LockdownIkm;

    let force = state
        .host_snapshot
        .lockdown_ikm_credential_rotation_requested;

    let mut txn = ctx.services.db_pool.begin().await?;

    // The site-wide target row is seeded for every site by the backfill migration
    // (and by `set_initial_target_version` on the first `RotateCredential`), so a
    // missing target is a corrupted invariant we surface rather than paper over.
    let target = db::credential_rotation::current_target_version(txn.as_mut(), LockdownIkm)
        .await
        .map_err(|e| StateHandlerError::GenericError(eyre!("read lockdown_ikm target: {e}")))?
        .ok_or_else(|| {
            StateHandlerError::GenericError(eyre!(
                "lockdown_ikm site-wide rotation target row is missing"
            ))
        })?;

    // Collect SVPC interfaces up front so the borrow of `state` does not overlap
    // the mutable `txn` borrow inside the loop.
    let svpc: Vec<_> = svpc_interfaces(state).collect();

    // Whether at least one card is still actively converging (kicked this tick or
    // mid-rekey within its step budget). While true the host waits; once false
    // every card has converged, been quarantined, or is untracked, so it settles.
    let mut in_progress = false;

    for iface in svpc {
        let nic_mac = iface.mac_address;
        let status =
            db::credential_rotation::device_rotation_status(txn.as_mut(), LockdownIkm, nic_mac)
                .await
                .map_err(|e| {
                    StateHandlerError::GenericError(eyre!(
                        "read lockdown_ikm status for {nic_mac}: {e}"
                    ))
                })?;

        // Untracked card (never locked / no bookkeeping row): not a rekey
        // candidate. Nothing to converge and nothing to quarantine.
        let Some(status) = status else {
            continue;
        };

        // Already at (or past) the target -- done.
        if status.converged {
            continue;
        }

        // A quarantined card does not block completion unless an operator forced
        // this rekey, in which case the force bypasses the backoff window.
        if status.quarantined && !force {
            continue;
        }

        match &iface.controller_state.value {
            DpaInterfaceControllerState::Ready => {
                // Idle and lagging: start the tenant-free rekey. CAS on the
                // controller-state version so a concurrent dpa-manager write
                // cannot be clobbered; a lost race just retries next tick.
                match db::dpa_interface::try_update_controller_state(
                    txn.as_mut(),
                    iface.id,
                    iface.controller_state.version,
                    iface.controller_state.version.increment(),
                    &DpaInterfaceControllerState::RotateKeyUnlocking,
                )
                .await
                .map_err(|e| {
                    StateHandlerError::GenericError(eyre!("kick rekey for {nic_mac}: {e}"))
                })? {
                    Applied(()) => tracing::info!(
                        %nic_mac,
                        dpa_interface_id = %iface.id,
                        target,
                        "starting SuperNIC lockdown IKM rekey to site-wide target"
                    ),
                    NotApplied(ControllerStateNotCurrent) => {}
                }
                in_progress = true;
            }
            DpaInterfaceControllerState::RotateKeyUnlocking
            | DpaInterfaceControllerState::RotateKeyLocking => {
                let elapsed = chrono::Utc::now()
                    .signed_duration_since(iface.controller_state.version.timestamp());

                // In the happy path, a card returns to Ready via the DPA state machine
                // This handles the failure path where the card is unreachable
                if elapsed > chrono::Duration::minutes(REKEY_STEP_TIMEOUT_MINUTES) {
                    // The card has not reported the expected lock mode within the
                    // step budget: treat it as unreachable, reset it to `Ready`,
                    // and quarantine it with backoff so the host can settle. The
                    // passive gate then skips the card until the window elapses; a
                    // later idle sweep retries it.
                    //
                    // Reset first and gate everything on the CAS. The snapshot may
                    // be stale: if dpa-manager advanced this card since it loaded
                    // (e.g. the card finally reported its lock mode), the CAS on
                    // the observed controller-state version loses.
                    match db::dpa_interface::try_update_controller_state(
                        txn.as_mut(),
                        iface.id,
                        iface.controller_state.version,
                        iface.controller_state.version.increment(),
                        &DpaInterfaceControllerState::Ready,
                    )
                    .await
                    .map_err(|e| {
                        StateHandlerError::GenericError(eyre!(
                            "rotation timed out; failed to transition {nic_mac} back to a ready state: {e}"
                        ))
                    })? {
                        Applied(()) => {
                            let quarantined_until = db::credential_rotation::backoff_until(
                                status.rotate_attempts,
                                chrono::Utc::now(),
                            );
                            db::credential_rotation::increment_rotate_attempt(
                                txn.as_mut(),
                                nic_mac,
                                LockdownIkm,
                                "SuperNIC lockdown rekey step timed out (card unreachable)",
                                quarantined_until,
                            )
                            .await
                            .map_err(|e| {
                                StateHandlerError::GenericError(eyre!(
                                    "quarantine unreachable rekey card {nic_mac}: {e}"
                                ))
                            })?;
                            tracing::warn!(
                                %nic_mac,
                                dpa_interface_id = %iface.id,
                                %quarantined_until,
                                "SuperNIC lockdown rekey step timed out; quarantined until backoff elapses"
                            );
                        }
                        NotApplied(ControllerStateNotCurrent) => {
                            // Lost the reset CAS: dpa-manager advanced the card since
                            // the snapshot loaded. Do not quarantine or settle on stale
                            // state; wait and re-read the card next tick.
                            in_progress = true;
                        }
                    }
                } else {
                    in_progress = true;
                }
            }
            // Transient assignment states should not appear on an idle host (the
            // entry guard is idle-only), but if one does, wait for it to settle
            // rather than forcing a transition; the host SLA backstops a wedge.
            other => {
                tracing::debug!(
                    %nic_mac,
                    dpa_interface_id = %iface.id,
                    state = %other,
                    "SuperNIC not idle during lockdown rekey; waiting"
                );
                in_progress = true;
            }
        }
    }

    if in_progress {
        // Commit the kicks / quarantine resets and keep waiting; the host state
        // does not change, so there is nothing to coordinate with a version bump.
        txn.commit().await.map_err(|e| {
            StateHandlerError::GenericError(eyre!("commit nic lockdown rekey progress: {e}"))
        })?;
        return Ok(StateHandlerOutcome::wait(
            "rekeying SuperNIC lockdown keys to the site-wide target".to_string(),
        ));
    }

    // Settled: every tracked SVPC card has converged or been attempted. Clear a
    // one-shot force request on the same transaction as the transition to Ready,
    // mirroring `clear_forced_bmc_requests`; a re-force is a fresh operator action.
    if force {
        db::machine::clear_lockdown_ikm_credential_rotation_requested(
            txn.as_mut(),
            state.host_snapshot.id,
        )
        .await
        .map_err(|e| {
            StateHandlerError::GenericError(eyre!("clear nic lockdown force request: {e}"))
        })?;
    }
    Ok(StateHandlerOutcome::transition(ManagedHostState::Ready).with_txn(txn))
}
