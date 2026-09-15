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

//! Machine-controller host UEFI credential rotation.
//!
//! The BMC sibling ([`super::rotation`]) delegates the whole password dance to
//! the shared [`carbide_credential_rotation`] engine because a BMC password
//! change is a single synchronous Redfish call. Host UEFI is different: applying
//! a new password requires a BIOS config job plus a full host power-cycle, so
//! the convergence itself is a multi-tick FSM
//! ([`super::handle_rotating_host_uefi`]). This module is the thin
//! policy/bookkeeping adapter around that FSM:
//!
//! - *Should we enter rotation?* [`should_enter_host_uefi_rotation`] honors an
//!   operator force-converge request unconditionally, else fires the passive
//!   [`RotationGate`](carbide_credential_rotation::RotationGate) only when UEFI
//!   rotation is enabled site-wide.
//! - *What credential authenticates the change?*
//!   [`host_uefi_current_candidates`] resolves the ordered, versioned
//!   current-password candidates -- the engine-free equivalent of the BMC
//!   `change_or_recover` candidate walk.
//! - *Clear the one-shot force request* once a forced rotation has run.
//!
//! Convergence/quarantine is recorded against the same
//! `device_credential_rotation` bookkeeping the BMC engine uses, keyed by the
//! host BMC MAC (mirroring the ingestion setup path and the backfill).
//!
//! This module rotates the host UEFI only. DPU UEFI rotation lives in its own
//! sibling `RotatingDpuUefi` state: it too stages a BIOS settings job and reboots
//! (the DPU, not the host), is keyed by the DPU BMC MAC, and a host can carry
//! several DPUs -- distinct enough from the host flow to keep the two states
//! separate rather than overloading this one.

use carbide_redfish::libredfish::CredentialOpError;
use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
use eyre::eyre;
use libredfish::{Redfish, SystemPowerControl};
use model::machine::{ManagedHostState, ManagedHostStateSnapshot, UefiSetupInfo, UefiSetupState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::{current_site_uefi_target, handler_host_power_control, resolve_site_uefi_credentials};
use crate::context::{MachineStateHandlerContextObjects, MachineStateHandlerServices};

/// Whether a Ready host should enter `ManagedHostState::RotatingHostUefi` now.
///
/// An operator force-converge request always wins -- the ops escape hatch is
/// honored even when the site-wide flag is off. Otherwise the passive gate fires
/// only when the host's initial BIOS password was already set
/// (`bios_password_set_time.is_some()`; a never-set host is an initial-setup
/// problem, not a rotation candidate), UEFI rotation is enabled site-wide, *and*
/// the host lags the staged target; the cheap checks are ordered first so a
/// disabled site (or a never-set host) never runs the gate query.
pub(crate) async fn should_enter_host_uefi_rotation(
    services: &MachineStateHandlerServices,
    mh: &ManagedHostStateSnapshot,
) -> Result<bool, StateHandlerError> {
    // An operator force-converge request drives entry on its own.
    if mh.host_snapshot.uefi_credential_rotation_requested {
        return Ok(true);
    }
    // Passive rotation only touches a host whose initial BIOS password NICo
    // actually set: `bios_password_set_time` is stamped on that success and
    // never cleared, so `is_some()` is a durable "we have driven this host's
    // UEFI password at least once" signal. A host that never got its initial
    // password set is an initial-setup problem (owned by the ingestion
    // `UefiSetup` flow, which for the tested Dell/Lenovo vendors intercepts such
    // a Ready host before this guard is reached), not a rotation candidate --
    // attempting to rotate it would just fail and land in backoff quarantine.
    // The operator force-converge escape hatch above still overrides this.
    if mh.host_snapshot.bios_password_set_time.is_none() {
        return Ok(false);
    }
    // Cheap site-wide flag first, so a disabled site never runs the gate query.
    if !services.site_config.uefi_rotation_enabled {
        return Ok(false);
    }
    // The host BMC MAC keys the host-UEFI rotation bookkeeping (mirrors the
    // ingestion setup path and the backfill). A host with no BMC MAC can be
    // neither tracked nor reached, so it never enters rotation.
    let Some(mac) = mh.host_snapshot.status.bmc_info.mac else {
        return Ok(false);
    };
    // The gate reports `true` when the host UEFI credential lags the staged
    // site-wide target and is not quarantined.
    services
        .host_uefi_rotation_gate
        .rotation_needed(&services.db_pool, mac)
        .await
        .map_err(|e| StateHandlerError::GenericError(eyre::eyre!("uefi rotation gate query: {e}")))
}

/// Ordered current-password candidates for a host UEFI rotation, most-likely
/// first: the secret at the device's tracked current version, then the target
/// version (covers an already-applied-but-unrecorded rotation), then the empty
/// string (a never-set host, or one reset to factory). The first that
/// authenticates the change wins. This is the engine-free equivalent of the BMC
/// `change_or_recover` candidate walk; because UEFI passwords are site-wide
/// uniform per version, the current credential is fully determined by the
/// tracked version -- no per-device secret is needed.
pub(crate) async fn host_uefi_current_candidates(
    reader: &dyn CredentialReader,
    current_version: Option<u32>,
    target_version: u32,
) -> Result<Vec<String>, StateHandlerError> {
    let mut versions: Vec<u32> = Vec::new();
    if let Some(v) = current_version {
        versions.push(v);
    }
    if !versions.contains(&target_version) {
        versions.push(target_version);
    }

    let mut candidates = Vec::with_capacity(versions.len() + 1);
    for version in versions {
        if let Some(password) = read_host_uefi_password(reader, version).await? {
            candidates.push(password);
        }
    }
    // Empty last: a never-set host, or one reset to the factory default.
    candidates.push(String::new());
    Ok(candidates)
}

/// Read the site-wide host UEFI password at a specific version, or `None` if no
/// secret is staged for that version.
async fn read_host_uefi_password(
    reader: &dyn CredentialReader,
    version: u32,
) -> Result<Option<String>, StateHandlerError> {
    let key = CredentialKey::host_uefi_site_default(version);
    let credentials = reader.get_credentials(&key).await.map_err(|e| {
        StateHandlerError::GenericError(eyre::eyre!(
            "read site host UEFI credential {}: {e}",
            key.to_key_str()
        ))
    })?;
    Ok(credentials.map(|Credentials::UsernamePassword { password, .. }| password))
}

/// A transition back into `RotatingHostUefi` at the next sub-state, carrying the
/// vendor BIOS job id (when one is being polled).
fn rotating_host_uefi_step(
    uefi_password_jid: Option<String>,
    uefi_setup_state: UefiSetupState,
) -> StateHandlerOutcome<ManagedHostState> {
    StateHandlerOutcome::transition(ManagedHostState::RotatingHostUefi {
        uefi_setup_info: UefiSetupInfo {
            uefi_password_jid,
            uefi_setup_state,
        },
    })
}

/// Multi-tick FSM converging a pool host's UEFI (BIOS setup) password to the
/// staged site-wide target in `ManagedHostState::RotatingHostUefi`.
///
/// Mirrors the ingestion [`handle_host_uefi_setup`] flow (unlock -> set -> wait
/// scheduled -> power-cycle -> wait complete -> record), but rotation-aware and
/// pool-only:
///  - the SET step authenticates with the *current-version* credential (via
///    [`host_uefi_current_candidates`]) rather than assuming the empty/factory
///    password, so a host at v1 rotating to v2 succeeds;
///  - convergence is recorded against the shared `device_credential_rotation`
///    bookkeeping keyed by the host BMC MAC (the same rows the BMC engine and
///    the ingestion path use); and
///  - every terminal path returns to `Ready`.
///
/// Failure handling keeps the host from wedging in the state: transient infra
/// errors return `Err` so the state framework retries (bounded by the state
/// SLA), while a device-level password-change failure is quarantined with
/// exponential backoff and returns to `Ready` (the passive gate then skips the
/// device until the window elapses).
pub(crate) async fn handle_rotating_host_uefi(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &ManagedHostStateSnapshot,
    uefi_setup_info: &UefiSetupInfo,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    // The host BMC MAC keys the rotation bookkeeping; without it the device can
    // be neither tracked nor reached, so refuse to act (the entry guard likewise
    // never selects such a host).
    let host_bmc_mac =
        state
            .host_snapshot
            .status
            .bmc_info
            .mac
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: state.host_snapshot.id.to_string(),
                missing: "bmc_mac",
            })?;

    match &uefi_setup_info.uefi_setup_state {
        UefiSetupState::UnlockHost => {
            if state.host_snapshot.needs_bmc_unlock_for_uefi_setup() {
                let redfish_client = ctx
                    .services
                    .create_redfish_client_from_machine(&state.host_snapshot)
                    .await?;
                redfish_client
                    .lockdown_bmc(libredfish::EnabledDisabled::Disabled)
                    .await
                    .map_err(|e| redfish_error("lockdown", e))?;
            }
            Ok(rotating_host_uefi_step(
                None,
                UefiSetupState::SetUefiPassword,
            ))
        }
        UefiSetupState::SetUefiPassword => {
            set_rotating_host_uefi_password(ctx, state, host_bmc_mac).await
        }
        UefiSetupState::WaitForPasswordJobScheduled => {
            if let Some(job_id) = uefi_setup_info.uefi_password_jid.as_ref() {
                let redfish_client = ctx
                    .services
                    .create_redfish_client_from_machine(&state.host_snapshot)
                    .await?;
                let job_state = redfish_client
                    .get_job_state(job_id)
                    .await
                    .map_err(|e| redfish_error("get_job_state", e))?;
                if !matches!(job_state, libredfish::JobState::Scheduled) {
                    return Ok(StateHandlerOutcome::wait(format!(
                        "waiting for UEFI rotation job {job_id:#?} to be scheduled; current state: {job_state:#?}"
                    )));
                }
            }
            Ok(rotating_host_uefi_step(
                uefi_setup_info.uefi_password_jid.clone(),
                UefiSetupState::PowercycleHost,
            ))
        }
        UefiSetupState::PowercycleHost => {
            handler_host_power_control(state, ctx, SystemPowerControl::ForceRestart).await?;
            Ok(rotating_host_uefi_step(
                uefi_setup_info.uefi_password_jid.clone(),
                UefiSetupState::WaitForPasswordJobCompletion,
            ))
        }
        UefiSetupState::WaitForPasswordJobCompletion => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&state.host_snapshot)
                .await?;
            if let Some(job_id) = uefi_setup_info.uefi_password_jid.as_ref() {
                let job_state = redfish_client
                    .get_job_state(job_id)
                    .await
                    .map_err(|e| redfish_error("get_job_state", e))?;
                if !matches!(job_state, libredfish::JobState::Completed) {
                    return Ok(StateHandlerOutcome::wait(format!(
                        "waiting for UEFI rotation job {job_id:#?} to complete; current state: {job_state:#?}"
                    )));
                }
            }
            finish_rotating_host_uefi(ctx, state, host_bmc_mac, redfish_client.as_ref()).await
        }
        // Deprecated ingestion-only step: the terminal `finish` re-enables the
        // BMC lockdown that `UnlockHost` disabled, so treat this as completion
        // for any host that somehow carries it.
        UefiSetupState::LockdownHost => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&state.host_snapshot)
                .await?;
            finish_rotating_host_uefi(ctx, state, host_bmc_mac, redfish_client.as_ref()).await
        }
    }
}

/// Stage and dispatch the host UEFI password change (the SET step). On a
/// successful dispatch, advance to the job-scheduling wait; on a device-level
/// failure, quarantine the device with backoff and return to `Ready`.
async fn set_rotating_host_uefi_password(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &ManagedHostStateSnapshot,
    host_bmc_mac: mac_address::MacAddress,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    use db::credential_rotation::CredentialRotationType::HostUefi;

    let db_pool = &ctx.services.db_pool;
    let reader = ctx.services.bmc_credential_ops.credential_reader();

    let target = current_site_uefi_target(db_pool, HostUefi).await?;

    // The device's tracked current version selects the first authentication
    // candidate; its prior attempt count sizes the backoff on failure.
    let (current_version, prior_attempts) = {
        let mut conn = db_pool.acquire().await?;
        match db::credential_rotation::device_rotation_status(&mut conn, HostUefi, host_bmc_mac)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("read host uefi rotation status: {e}"))
            })? {
            Some(status) => (
                status.current_version.and_then(|v| u32::try_from(v).ok()),
                status.rotate_attempts,
            ),
            None => (None, 0),
        }
    };

    let candidates = host_uefi_current_candidates(reader, current_version, target).await?;
    let Credentials::UsernamePassword {
        password: new_password,
        ..
    } = resolve_site_uefi_credentials(db_pool, reader, HostUefi).await?;

    // Stage the target before dispatch (crash-safe), in its own short
    // transaction so no lock is held across the Redfish round-trip.
    {
        let mut conn = db_pool.acquire().await?;
        db::credential_rotation::mark_device_rotating_to_version(
            &mut conn,
            host_bmc_mac,
            HostUefi,
            target as i32,
        )
        .await
        .map_err(|e| {
            StateHandlerError::GenericError(eyre!("stage host uefi rotating_to_version: {e}"))
        })?;
    }

    let access = ctx
        .services
        .bmc_access_info_for_machine(&state.host_snapshot)
        .await?;
    match ctx
        .services
        .bmc_credential_ops
        .rotate_uefi_password(&access, &candidates, new_password)
        .await
    {
        Ok(job_id) => Ok(rotating_host_uefi_step(
            job_id,
            UefiSetupState::WaitForPasswordJobScheduled,
        )),
        // Client creation failed (credential store, TCP, or the vendor
        // probe): return Err so the state framework retries; quarantine is
        // reserved for the operation itself failing on the device.
        Err(CredentialOpError::ClientCreation(e)) => Err(e.into()),
        Err(CredentialOpError::Operation(e)) => {
            // Device-level failure (all current-password candidates rejected, or
            // the vendor refused the change). The pool already redacted the
            // password out of the error. Quarantine with backoff and return to
            // Ready so the host never wedges in the state.
            let redacted = e.to_string();
            let quarantined_until =
                db::credential_rotation::backoff_until(prior_attempts, chrono::Utc::now());
            // Restore the BMC lockdown that `UnlockHost` disabled: the host
            // returns to Ready and must not be left unlocked just because the
            // rotation failed. Best-effort and outside the quarantine txn -- a
            // re-lock failure must neither hold a lock across a Redfish call nor
            // mask the quarantine record (which would tighten the retry loop), so
            // we log and continue.
            let relock = async {
                let client = ctx
                    .services
                    .redfish_client_pool
                    .client_by_info(&access)
                    .await
                    .map_err(StateHandlerError::from)?;
                reenable_host_bmc_lockdown_after_rotation(state, client.as_ref()).await
            };
            if let Err(relock_err) = relock.await {
                tracing::warn!(
                    mac = %host_bmc_mac,
                    error = %relock_err,
                    "failed to re-enable BMC lockdown after a failed host UEFI rotation; host left unlocked"
                );
            }
            let mut txn = db_pool.begin().await?;
            db::credential_rotation::increment_rotate_attempt(
                &mut txn,
                host_bmc_mac,
                HostUefi,
                &redacted,
                quarantined_until,
            )
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("record host uefi rotation failure: {e}"))
            })?;
            tracing::warn!(
                mac = %host_bmc_mac,
                %quarantined_until,
                error = %redacted,
                "host UEFI rotation attempt failed; quarantined until backoff elapses"
            );
            // A forced attempt genuinely fired, so clear the one-shot request on
            // the same transaction; a re-force is a fresh operator action.
            if state.host_snapshot.uefi_credential_rotation_requested {
                db::machine::clear_uefi_credential_rotation_requested(
                    &mut txn,
                    state.host_snapshot.id.into(),
                )
                .await?;
            }
            Ok(StateHandlerOutcome::transition(ManagedHostState::Ready).with_txn(txn))
        }
    }
}

/// Re-enable the Dell BMC System Lockdown that the `UnlockHost` step disabled to
/// apply the UEFI password, so a rotated host returns to `Ready` with the same
/// lockdown posture it had before. Mirrors the unlock exactly -- `lockdown_bmc`,
/// Dell only -- and honors the `disable_lockdown` host profile (a host
/// configured to stay unlocked is left unlocked, matching the ingestion
/// `WaitingForLockdown`/`SetLockdown` guard). Unlike the ingestion re-lock, BMC
/// lockdown is a live manager attribute, so no reboot is required here.
async fn reenable_host_bmc_lockdown_after_rotation(
    state: &ManagedHostStateSnapshot,
    redfish_client: &dyn Redfish,
) -> Result<(), StateHandlerError> {
    if state.host_snapshot.needs_bmc_unlock_for_uefi_setup()
        && !state.host_snapshot.host_profile.disable_lockdown
    {
        redfish_client
            .lockdown_bmc(libredfish::EnabledDisabled::Enabled)
            .await
            .map_err(|e| redfish_error("lockdown", e))?;
    }
    Ok(())
}

/// Record host UEFI convergence and return to `Ready`. First re-enables the BMC
/// lockdown that `UnlockHost` disabled (retry-safe: on failure the tick errors
/// and re-enters here, since the completed job still reports `Completed`).
/// Promotes the staged `rotating_to_version`; for a row predating the staged
/// flow (no marker), falls back to
/// [`record_device_converged`](db::credential_rotation::record_device_converged),
/// mirroring the BMC engine. Clears a one-shot force request on the same
/// transaction.
async fn finish_rotating_host_uefi(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &ManagedHostStateSnapshot,
    host_bmc_mac: mac_address::MacAddress,
    redfish_client: &dyn Redfish,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    use db::credential_rotation::CredentialRotationType::HostUefi;

    reenable_host_bmc_lockdown_after_rotation(state, redfish_client).await?;

    let mut txn = ctx.services.db_pool.begin().await?;
    let promoted =
        db::credential_rotation::promote_rotating_to_current(&mut txn, host_bmc_mac, HostUefi)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("promote host uefi rotating_to_version: {e}"))
            })?;
    if !promoted {
        db::credential_rotation::record_device_converged(&mut txn, host_bmc_mac, HostUefi)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("record host uefi convergence: {e}"))
            })?;
    }
    tracing::info!(mac = %host_bmc_mac, "host UEFI converged to site-wide rotation target");
    if state.host_snapshot.uefi_credential_rotation_requested {
        db::machine::clear_uefi_credential_rotation_requested(
            &mut txn,
            state.host_snapshot.id.into(),
        )
        .await?;
    }
    Ok(StateHandlerOutcome::transition(ManagedHostState::Ready).with_txn(txn))
}

#[cfg(test)]
mod tests {
    use carbide_secrets::MemoryCredentialStore;
    use carbide_secrets::credentials::CredentialWriter;

    use super::*;

    /// Seed a versioned host UEFI secret into an in-memory reader.
    async fn seed(store: &MemoryCredentialStore, version: u32, password: &str) {
        store
            .set_credentials(
                &CredentialKey::host_uefi_site_default(version),
                &Credentials::UsernamePassword {
                    username: String::new(),
                    password: password.to_string(),
                },
            )
            .await
            .expect("seeding a host UEFI secret should succeed");
    }

    /// A tracked host lists its current-version secret first, then the target
    /// version, then the empty string, so authentication tries the most likely
    /// current password before falling back.
    #[tokio::test]
    async fn candidates_are_current_then_target_then_empty() {
        let store = MemoryCredentialStore::default();
        seed(&store, 1, "current-v1").await;
        seed(&store, 2, "target-v2").await;

        let candidates = host_uefi_current_candidates(&store, Some(1), 2)
            .await
            .expect("resolving candidates should succeed");

        assert_eq!(
            candidates,
            vec![
                "current-v1".to_string(),
                "target-v2".to_string(),
                String::new(),
            ],
        );
    }

    /// A never-rotated host (no tracked current version) tries the target then
    /// the empty string -- the first-rotation empty-vs-versioned case.
    #[tokio::test]
    async fn candidates_for_a_never_rotated_host_are_target_then_empty() {
        let store = MemoryCredentialStore::default();
        seed(&store, 0, "legacy-v0").await;

        let candidates = host_uefi_current_candidates(&store, None, 0)
            .await
            .expect("resolving candidates should succeed");

        assert_eq!(candidates, vec!["legacy-v0".to_string(), String::new()],);
    }

    /// When current and target are the same version it is listed once, so we
    /// never probe the same password twice.
    #[tokio::test]
    async fn candidates_dedupe_when_current_equals_target() {
        let store = MemoryCredentialStore::default();
        seed(&store, 3, "v3").await;

        let candidates = host_uefi_current_candidates(&store, Some(3), 3)
            .await
            .expect("resolving candidates should succeed");

        assert_eq!(candidates, vec!["v3".to_string(), String::new()]);
    }
}
