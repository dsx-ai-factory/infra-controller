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

use bmc_backend::BackendBmc;
use bmc_platform::{
    DriverOutcome, LockdownDesiredState, LockdownScope, OperationStatus, PlatformError, ResetType,
};
use carbide_redfish::state_handler::state_handler_platform_error as platform_error;
use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
use db::TransactionVending as _;
use db::credential_rotation::NoStagedCredentialRotation;
use eyre::eyre;
use model::machine::{ManagedHostState, ManagedHostStateSnapshot, UefiSetupInfo, UefiSetupState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::{current_site_uefi_target, handler_host_power_control, read_site_uefi_credentials};
use crate::context::{MachineStateHandlerContextObjects, MachineStateHandlerServices};
use crate::platform_config;
use crate::redfish::poll_idrac_job;

/// Walks `candidates` in order against `bmc.bios()?.change_uefi_password(...)`,
/// returning the first [`DriverOutcome`] that succeeds. Used by both host and
/// DPU UEFI password rotation.
pub(super) async fn rotate_uefi_password_candidates(
    bmc: &BackendBmc,
    candidates: &[String],
    new_password: &str,
) -> Result<DriverOutcome, PlatformError> {
    let bios = bmc.bios().map_err(PlatformError::from)?;
    let mut last_err = None;
    for candidate in candidates {
        match bios.change_uefi_password(candidate, new_password).await {
            Ok(outcome) => return Ok(outcome),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "UEFI password change failed for a current-password candidate; trying the next"
                );
                last_err = Some(error);
            }
        }
    }
    Err(last_err.expect("rotate_uefi_password_candidates requires at least one candidate"))
}
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
            credential_version: None,
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
/// exponential backoff and returns to `Ready` after restoring required Lenovo
/// BMC locks (the passive gate then skips the device until the window elapses).
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
                let bmc = ctx.services.connect_bmc(&state.host_snapshot).await?;
                bmc.set_lockdown(LockdownScope::Bmc, LockdownDesiredState::Disabled)
                    .await
                    .map_err(|e| platform_error("lockdown", e))?;
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
                let status = poll_idrac_job(ctx.services, &state.host_snapshot, job_id).await?;
                if !matches!(status, OperationStatus::AwaitingReset) {
                    return Ok(StateHandlerOutcome::wait(format!(
                        "waiting for UEFI rotation job {job_id} to be scheduled; current state: {status:?}"
                    )));
                }
            }
            Ok(rotating_host_uefi_step(
                uefi_setup_info.uefi_password_jid.clone(),
                UefiSetupState::PowercycleHost,
            ))
        }
        UefiSetupState::PowercycleHost => {
            handler_host_power_control(state, ctx, ResetType::ForceRestart).await?;
            Ok(rotating_host_uefi_step(
                uefi_setup_info.uefi_password_jid.clone(),
                UefiSetupState::WaitForPasswordJobCompletion,
            ))
        }
        UefiSetupState::WaitForPasswordJobCompletion => {
            if let Some(job_id) = uefi_setup_info.uefi_password_jid.as_ref() {
                let status = poll_idrac_job(ctx.services, &state.host_snapshot, job_id).await?;
                if !matches!(status, OperationStatus::Completed) {
                    return Ok(StateHandlerOutcome::wait(format!(
                        "waiting for UEFI rotation job {job_id} to complete; current state: {status:?}"
                    )));
                }
            }
            finish_rotating_host_uefi(ctx, state, host_bmc_mac).await
        }
        // Lenovo failure cleanup retries only the re-lock. The failed rotation
        // is already quarantined; do not promote its credential version.
        UefiSetupState::LockdownHost => {
            if state.host_snapshot.bmc_vendor().is_lenovo() {
                reenable_host_bmc_lockdown_after_rotation(ctx.services, state).await?;
                Ok(StateHandlerOutcome::transition(ManagedHostState::Ready))
            } else {
                finish_rotating_host_uefi(ctx, state, host_bmc_mac).await
            }
        }
    }
}

/// Stage and dispatch the host UEFI password change (the SET step). On a
/// successful dispatch, advance to the job-scheduling wait; on a device-level
/// failure, quarantine with backoff and restore required Lenovo BMC locks
/// before returning to `Ready`.
async fn set_rotating_host_uefi_password(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &ManagedHostStateSnapshot,
    host_bmc_mac: mac_address::MacAddress,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    use db::credential_rotation::CredentialRotationType::HostUefi;

    let db_pool = &ctx.services.db_pool;
    let reader = ctx.services.credential_manager.as_ref();

    let target = current_site_uefi_target(db_pool, HostUefi).await?;

    // The device's tracked current version selects the first authentication
    // candidate; its prior attempt count sizes the backoff on failure.
    let (current_version, prior_attempts) =
        match db::credential_rotation::device_rotation_status(db_pool, HostUefi, host_bmc_mac)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("read host uefi rotation status: {e}"))
            })? {
            Some(status) => (
                status.current_version.and_then(|v| u32::try_from(v).ok()),
                status.rotate_attempts,
            ),
            None => (None, 0),
        };

    let candidates = host_uefi_current_candidates(reader, current_version, target).await?;
    let Credentials::UsernamePassword {
        password: new_password,
        ..
    } = read_site_uefi_credentials(reader, HostUefi, target).await?;

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

    // Connection failures return Err so the state framework retries; quarantine
    // is reserved for the password-change operation itself failing on the device.
    let bmc = ctx.services.connect_bmc(&state.host_snapshot).await?;
    match rotate_uefi_password_candidates(&bmc, &candidates, &new_password).await {
        Ok(outcome) => {
            let job_id = platform_config::vendor_job_id(&outcome).map(|id| id.to_string());
            Ok(rotating_host_uefi_step(
                job_id,
                UefiSetupState::WaitForPasswordJobScheduled,
            ))
        }
        Err(e) => {
            // Device-level failure (all current-password candidates rejected, or
            // the vendor refused the change). Passwords are already redacted out
            // of the error. Quarantine with backoff and return to Ready so the
            // host never wedges in the state.
            let redacted = e.to_string();
            let quarantined_until =
                db::credential_rotation::backoff_until(prior_attempts, chrono::Utc::now());
            // Restore the BMC lockdown that `UnlockHost` disabled: the host
            // returns to Ready and must not be left unlocked just because the
            // rotation failed. Keep the network call outside the quarantine txn.
            // A Lenovo re-lock failure persists a cleanup step with the failed
            // rotation record, so retries cannot repeat the password change.
            let relock_result =
                reenable_host_bmc_lockdown_after_rotation(ctx.services, state).await;
            if let Err(relock_err) = &relock_result {
                tracing::warn!(
                    mac = %host_bmc_mac,
                    error = %relock_err,
                    "failed to re-enable BMC lockdown after a failed host UEFI rotation"
                );
            }
            let mut txn = db_pool
                .txn_begin()
                .await
                .map_err(StateHandlerError::from_transaction)?;
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
            let outcome = if relock_result.is_err() && state.host_snapshot.bmc_vendor().is_lenovo()
            {
                rotating_host_uefi_step(None, UefiSetupState::LockdownHost)
            } else {
                StateHandlerOutcome::transition(ManagedHostState::Ready)
            };
            Ok(outcome.with_txn(txn))
        }
    }
}

/// Re-enable the BMC lockdown that the `UnlockHost` step disabled to
/// apply the UEFI password, so a rotated host returns to `Ready` with the same
/// lockdown posture it had before. Mirrors the unlock exactly -- `lockdown_bmc`,
/// for BMCs that can lock BIOS settings -- and honors the `disable_lockdown`
/// host profile (a host configured to stay unlocked is left unlocked, matching the ingestion
/// `WaitingForLockdown`/`SetLockdown` guard).
async fn reenable_host_bmc_lockdown_after_rotation(
    services: &MachineStateHandlerServices,
    state: &ManagedHostStateSnapshot,
) -> Result<(), StateHandlerError> {
    if state.host_snapshot.needs_bmc_unlock_for_uefi_setup()
        && !state.host_snapshot.host_profile.disable_lockdown
    {
        let bmc = services.connect_bmc(&state.host_snapshot).await?;
        bmc.set_lockdown(LockdownScope::Bmc, LockdownDesiredState::Enabled)
            .await
            .map_err(|e| platform_error("lockdown", e))?;
    }
    Ok(())
}

/// Record host UEFI convergence and return to `Ready`. First re-enables the BMC
/// lockdown that `UnlockHost` disabled (retry-safe: on failure the tick errors
/// and re-enters here, since the completed job still reports `Completed`).
/// Promotes the staged `rotating_to_version` and clears a one-shot force request
/// in the same transaction. Without a staged version, keep the existing record:
/// job completion alone does not identify the password version.
async fn finish_rotating_host_uefi(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &ManagedHostStateSnapshot,
    host_bmc_mac: mac_address::MacAddress,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    use db::credential_rotation::CredentialRotationType::HostUefi;

    reenable_host_bmc_lockdown_after_rotation(ctx.services, state).await?;

    let mut txn = ctx
        .services
        .db_pool
        .txn_begin()
        .await
        .map_err(StateHandlerError::from_transaction)?;
    let promoted =
        db::credential_rotation::promote_rotating_to_current(&mut txn, host_bmc_mac, HostUefi)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("promote host uefi rotating_to_version: {e}"))
            })?;
    if let db::ConditionalWrite::NotApplied(NoStagedCredentialRotation) = promoted {
        tracing::warn!(
            mac = %host_bmc_mac,
            "host UEFI job completed without a staged credential version; keeping existing rotation bookkeeping"
        );
    } else {
        tracing::info!(
            mac = %host_bmc_mac,
            "host UEFI converged to its staged credential version"
        );
    }
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
    use std::sync::{Arc, Mutex};

    use arc_swap::ArcSwap;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use bmc_backend::BackendBmc;
    use bmc_mock::test_support::serve_https;
    use bmc_platform::{OperationReference, VendorJobId};
    use bmc_runtime::{DriverConnector, DriverPools};
    use carbide_redfish::nv_redfish::new_pool;
    use carbide_secrets::MemoryCredentialStore;
    use carbide_secrets::credentials::CredentialWriter;
    use carbide_utils::redfish::BmcAccessInfo;
    use mac_address::MacAddress;
    use model::site_explorer::{
        ComputerSystem, EndpointExplorationReport, Manager, ServiceRoot as ModelServiceRoot,
    };
    use serde_json::{Value, json};

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

    async fn connect_drivers_mock(
        name: &str,
        router: Router,
        report: EndpointExplorationReport,
    ) -> (bmc_mock::CombinedServer, BackendBmc) {
        let (server, base_url) = serve_https(name, router);
        let pool = new_pool(Arc::new(ArcSwap::from_pointee(None)));
        let store = Arc::new(MemoryCredentialStore::default());
        let mac = MacAddress::new([2, 0, 0, 0, 0, 42]);
        store
            .set_credentials(
                &CredentialKey::BmcCredentials {
                    credential_type: carbide_secrets::credentials::BmcCredentialType::BmcRoot {
                        bmc_mac_address: mac,
                    },
                },
                &Credentials::new("root", "bmc-secret"),
            )
            .await
            .expect("seed bmc root");
        let drivers = DriverConnector::new(
            DriverPools {
                routed: pool.clone(),
                direct: pool,
            },
            store,
            "uefi-rotation-test",
            bmc_drivers::built_in_rules(),
            carbide_ipmi::test_support(),
            carbide_instrument::Instrumentation::noop(),
        )
        .expect("driver connector builds");
        let access = BmcAccessInfo {
            host: "127.0.0.1".to_string(),
            port: base_url.port(),
            mac_address: mac,
        };
        let connected = drivers
            .connect(&access, &report)
            .await
            .expect("drivers backend connects");
        (server, BackendBmc::from(connected))
    }

    /// Host UEFI rotation through `Bios::change_uefi_password` walks
    /// candidates in order on the drivers backend, skips rejected candidates,
    /// returns the vendor job reference from `DriverOutcome`, and redacts all
    /// passwords when every candidate fails.
    #[tokio::test]
    async fn host_uefi_candidate_walk_uses_bios_driver_and_redacts_failures() {
        let attempts: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let attempts_for_route = attempts.clone();

        let router = Router::new()
            .route(
                "/redfish/v1",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1",
                        "Id": "RootService",
                        "Name": "Root Service",
                        "Vendor": "Dell",
                        "Systems": {"@odata.id": "/redfish/v1/Systems"},
                        "Managers": {"@odata.id": "/redfish/v1/Managers"},
                        "Links": {"Sessions": {"@odata.id": "/redfish/v1/SessionService/Sessions"}},
                    }))
                }),
            )
            .route(
                "/redfish/v1/Systems",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Systems",
                        "@odata.type": "#ComputerSystemCollection.ComputerSystemCollection",
                        "Name": "Systems",
                        "Members": [{"@odata.id": "/redfish/v1/Systems/System.Embedded.1"}],
                    }))
                }),
            )
            .route(
                "/redfish/v1/Systems/System.Embedded.1",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Systems/System.Embedded.1",
                        "@odata.type": "#ComputerSystem.v1_20_0.ComputerSystem",
                        "Id": "System.Embedded.1",
                        "Name": "System",
                        "Bios": {"@odata.id": "/redfish/v1/Systems/System.Embedded.1/Bios"},
                    }))
                }),
            )
            .route(
                "/redfish/v1/Systems/System.Embedded.1/Bios",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Systems/System.Embedded.1/Bios",
                        "@odata.type": "#Bios.v1_2_0.Bios",
                        "Id": "Bios",
                        "Name": "BIOS",
                        "@Redfish.Settings": {
                            "SettingsObject": {"@odata.id": "/redfish/v1/Systems/System.Embedded.1/Bios/Settings"}
                        },
                        "Actions": {
                            "#Bios.ChangePassword": {
                                "target": "/redfish/v1/Systems/System.Embedded.1/Bios/Actions/Bios.ChangePassword"
                            }
                        }
                    }))
                }),
            )
            .route(
                "/redfish/v1/Systems/System.Embedded.1/Bios/Actions/Bios.ChangePassword",
                post({
                    let attempts = attempts_for_route;
                    move |Json(body): Json<Value>| {
                        let attempts = attempts.clone();
                        async move {
                            let old = body["OldPassword"].as_str().unwrap_or_default().to_string();
                            let new = body["NewPassword"].as_str().unwrap_or_default().to_string();
                            attempts.lock().unwrap().push((old.clone(), new.clone()));
                            if old == "current-v1" {
                                (StatusCode::NO_CONTENT, Json(json!({})))
                            } else {
                                (
                                    StatusCode::BAD_REQUEST,
                                    Json(json!({
                                        "error": {
                                            "code": "Base.1.12.GeneralError",
                                            "message": format!("bad password old={old} new={new}")
                                        }
                                    })),
                                )
                            }
                        }
                    }
                }),
            )
            .route(
                "/redfish/v1/Managers",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Managers",
                        "@odata.type": "#ManagerCollection.ManagerCollection",
                        "Name": "Managers",
                        "Members": [{"@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1"}],
                    }))
                }),
            )
            .route(
                "/redfish/v1/Managers/iDRAC.Embedded.1",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1",
                        "@odata.type": "#Manager.v1_18_0.Manager",
                        "Id": "iDRAC.Embedded.1",
                        "Name": "Manager",
                        "Links": {"Oem": {"Dell": {
                            "DellAttributes": [{"@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1"}],
                            "DellJobService": {"@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService"},
                            "Jobs": {"@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs"},
                        }}}
                    }))
                }),
            )
            .route(
                "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellAttributes/iDRAC.Embedded.1",
                        "Id": "iDRAC.Embedded.1",
                        "Name": "Manager Attributes",
                        "Attributes": {"Lockdown.1.SystemLockdown": "Disabled"},
                    }))
                }),
            )
            .route(
                "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService",
                        "@odata.type": "#DellJobService.v1_2_0.DellJobService",
                        "Id": "DellJobService",
                        "Name": "DellJobService",
                        "Actions": {
                            "#DellJobService.DeleteJobQueue": {
                                "target": "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService/Actions/DellJobService.DeleteJobQueue"
                            }
                        }
                    }))
                }),
            )
            .route(
                "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/DellJobService/Actions/DellJobService.DeleteJobQueue",
                post(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs",
                post(|| async {
                    (
                        StatusCode::ACCEPTED,
                        [(
                            axum::http::header::LOCATION,
                            "/redfish/v1/Managers/iDRAC.Embedded.1/Oem/Dell/Jobs/JID_999",
                        )],
                        Json(json!({})),
                    )
                }),
            );

        let report = EndpointExplorationReport {
            service_root: Some(ModelServiceRoot {
                vendor: Some("Dell".to_string()),
                ..Default::default()
            }),
            systems: vec![ComputerSystem {
                id: "System.Embedded.1".to_string(),
                ..Default::default()
            }],
            managers: vec![Manager {
                id: "iDRAC.Embedded.1".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let (_server, bmc) = connect_drivers_mock("host-uefi-drivers", router, report).await;

        // First candidate fails, second succeeds and yields the iDRAC job.
        let outcome = rotate_uefi_password_candidates(
            &bmc,
            &["stale-v0".to_string(), "current-v1".to_string()],
            "target-v2",
        )
        .await
        .expect("second candidate succeeds");
        assert_eq!(
            platform_config::vendor_job_id(&outcome),
            Some(VendorJobId::new("JID_999".to_string()).unwrap())
        );
        assert!(matches!(
            outcome.references().next(),
            Some(OperationReference::VendorJob { .. })
        ));
        assert_eq!(
            *attempts.lock().unwrap(),
            vec![
                ("stale-v0".to_string(), "target-v2".to_string()),
                ("current-v1".to_string(), "target-v2".to_string()),
            ]
        );

        // When every candidate fails, neither candidate nor new password leaks.
        let err = rotate_uefi_password_candidates(&bmc, &["wrong-pw".to_string()], "secret-new")
            .await
            .expect_err("all candidates rejected");
        let rendered = err.to_string();
        assert!(
            !rendered.contains("wrong-pw"),
            "candidate leaked: {rendered}"
        );
        assert!(
            !rendered.contains("secret-new"),
            "new password leaked: {rendered}"
        );
    }

    /// DPU UEFI rotation through `Bios::change_uefi_password` walks candidates
    /// against `Bios/Settings` on the BlueField driver and completes without a job.
    #[tokio::test]
    async fn dpu_uefi_candidate_walk_uses_bluefield_bios_driver() {
        let router = Router::new()
            .route(
                "/redfish/v1",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1",
                        "Id": "RootService",
                        "Name": "Root Service",
                        "Vendor": "Nvidia",
                        "Product": "BlueField-3 DPU",
                        "Systems": {"@odata.id": "/redfish/v1/Systems"},
                        "Managers": {"@odata.id": "/redfish/v1/Managers"},
                        "Links": {"Sessions": {"@odata.id": "/redfish/v1/SessionService/Sessions"}},
                    }))
                }),
            )
            .route(
                "/redfish/v1/Systems",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Systems",
                        "@odata.type": "#ComputerSystemCollection.ComputerSystemCollection",
                        "Name": "Systems",
                        "Members": [{"@odata.id": "/redfish/v1/Systems/Bluefield"}],
                    }))
                }),
            )
            .route(
                "/redfish/v1/Systems/Bluefield",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Systems/Bluefield",
                        "@odata.type": "#ComputerSystem.v1_20_0.ComputerSystem",
                        "Id": "Bluefield",
                        "Name": "Bluefield",
                        "Bios": {"@odata.id": "/redfish/v1/Systems/Bluefield/Bios"},
                    }))
                }),
            )
            .route(
                "/redfish/v1/Systems/Bluefield/Bios",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Systems/Bluefield/Bios",
                        "@odata.type": "#Bios.v1_2_0.Bios",
                        "Id": "BIOS",
                        "Name": "BIOS",
                        "@Redfish.Settings": {
                            "SettingsObject": {"@odata.id": "/redfish/v1/Systems/Bluefield/Bios/Settings"}
                        },
                        "Attributes": {}
                    }))
                }),
            )
            .route(
                "/redfish/v1/Systems/Bluefield/Bios/Settings",
                get(|| async {
                    Json(json!({
                        "@odata.id": "/redfish/v1/Systems/Bluefield/Bios/Settings",
                        "@odata.type": "#Bios.v1_2_0.Bios",
                        "Id": "BIOS_Settings",
                        "Name": "BIOS Settings",
                        "Attributes": {}
                    }))
                })
                .patch(|Json(body): Json<Value>| async move {
                    if body["Attributes"]["CurrentUefiPassword"] == json!("bluefield") {
                        StatusCode::OK
                    } else {
                        StatusCode::BAD_REQUEST
                    }
                }),
            );

        let report = EndpointExplorationReport {
            service_root: Some(ModelServiceRoot {
                vendor: Some("Nvidia".to_string()),
                product: Some("BlueField-3 DPU".to_string()),
                ..Default::default()
            }),
            systems: vec![ComputerSystem {
                id: "Bluefield".to_string(),
                ..Default::default()
            }],
            managers: vec![Manager {
                id: "Bluefield_BMC".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let (_server, bmc) = connect_drivers_mock("dpu-uefi-drivers", router, report).await;
        let outcome = rotate_uefi_password_candidates(
            &bmc,
            &["stale-v1".to_string(), "bluefield".to_string()],
            "dpu-v2",
        )
        .await
        .expect("BlueField factory fallback succeeds");
        assert_eq!(outcome, DriverOutcome::complete());
    }
}
