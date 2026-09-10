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

//! Machine-controller DPU UEFI credential rotation.
//!
//! The DPU sibling of [`super::host_uefi_rotation`]. A DPU's UEFI password is a
//! distinct device from its host's: it is keyed by the *DPU* BMC MAC, applied
//! through a DPU restart (not a host power-cycle), and a host can carry several
//! DPUs. So DPU UEFI convergence gets its own [`RotatingDpuUefi`] state
//! ([`handle_rotating_dpu_uefi`]) that converges *one* DPU per
//! `Ready -> RotatingDpuUefi -> Ready` cycle; this module is the thin
//! policy/bookkeeping adapter around that FSM:
//!
//! - *Which DPU (if any) should rotate now?* [`select_dpu_for_uefi_rotation`]
//!   returns the first DPU that is force-requested (operator escape hatch,
//!   honored even when the site flag is off) or -- when UEFI rotation is enabled
//!   site-wide -- lags the staged `dpu_uefi` target. A DPU with no
//!   `device_credential_rotation` row (never set) is skipped by the passive gate
//!   exactly as a never-set host is, since [`RotationGate::rotation_needed`]
//!   reports no work for a missing row; a force request still selects it.
//! - *What credential authenticates the change?*
//!   [`dpu_uefi_current_candidates`] resolves the ordered, versioned
//!   current-password candidates -- the DPU analogue of the host candidate walk,
//!   ending in the DPU factory default rather than the empty string.
//!
//! [`RotatingDpuUefi`]: model::machine::ManagedHostState::RotatingDpuUefi
//! [`RotationGate::rotation_needed`]: carbide_credential_rotation::RotationGate::rotation_needed

use bmc_vendor::DpuModel;
use carbide_redfish::libredfish::CredentialOpError;
use carbide_secrets::credentials::{CredentialKey, CredentialReader, CredentialType, Credentials};
use carbide_uuid::machine::DpuMachineId;
use eyre::eyre;
use model::machine::{DpuMachine, ManagedHostState, ManagedHostStateSnapshot};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::{current_site_uefi_target, handler_restart_dpu, resolve_site_uefi_credentials};
use crate::context::{MachineStateHandlerContextObjects, MachineStateHandlerServices};

/// `true` when this DPU's UEFI credential lags the staged site-wide `dpu_uefi`
/// target and is not quarantined. A DPU with no BMC MAC (untrackable) or no
/// rotation row (never set) yields `false`.
async fn dpu_uefi_rotation_needed(
    services: &MachineStateHandlerServices,
    dpu: &DpuMachine,
) -> Result<bool, StateHandlerError> {
    let Some(mac) = dpu.status.bmc_info.mac else {
        return Ok(false);
    };
    services
        .dpu_uefi_rotation_gate
        .rotation_needed(&services.db_pool, mac)
        .await
        .map_err(|e| {
            StateHandlerError::GenericError(eyre::eyre!("dpu uefi rotation gate query: {e}"))
        })
}

/// Whether this DPU should enter `RotatingDpuUefi` now. An operator
/// force-converge request always wins -- honored even when the site-wide flag is
/// off. Otherwise the passive gate fires only when UEFI rotation is enabled
/// site-wide *and* the DPU lags the staged target; the cheap flag is checked
/// first so a disabled site never runs the gate query.
async fn should_rotate_dpu_uefi(
    services: &MachineStateHandlerServices,
    dpu: &DpuMachine,
) -> Result<bool, StateHandlerError> {
    if dpu.uefi_credential_rotation_requested {
        return Ok(true);
    }
    Ok(services.site_config.uefi_rotation_enabled
        && dpu_uefi_rotation_needed(services, dpu).await?)
}

/// Select the DPU (if any) a Ready host should converge this cycle, in
/// `dpu_snapshots` order: the first DPU that is force-requested or (with UEFI
/// rotation enabled site-wide) lags the staged `dpu_uefi` target. Returns the
/// DPU's machine id, which `RotatingDpuUefi` carries to key the reboot FSM to
/// that one DPU; the remaining DPUs are re-selected on later sweeps, one per
/// `Ready -> RotatingDpuUefi -> Ready` cycle.
pub(crate) async fn select_dpu_for_uefi_rotation(
    services: &MachineStateHandlerServices,
    mh: &ManagedHostStateSnapshot,
) -> Result<Option<DpuMachineId>, StateHandlerError> {
    for dpu in &mh.dpu_snapshots {
        if should_rotate_dpu_uefi(services, dpu).await? {
            return Ok(Some(dpu.id));
        }
    }
    Ok(None)
}

/// Ordered current-password candidates for a DPU UEFI rotation, most-likely
/// first: the secret at the device's tracked current version, then the target
/// version (covers an already-applied-but-unrecorded rotation), then the DPU
/// factory default. The first that authenticates the change wins. This is the
/// DPU analogue of the host candidate walk, differing only in the terminal
/// fallback: a never-rotated DPU still carries its hardware factory password
/// (e.g. "bluefield"), not the empty string a factory-reset host carries.
pub(crate) async fn dpu_uefi_current_candidates(
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
        if let Some(password) = read_dpu_uefi_password(reader, version).await? {
            candidates.push(password);
        }
    }
    // Factory default last: a never-set DPU still holds its hardware password.
    candidates.push(read_dpu_factory_default(reader).await?);
    Ok(candidates)
}

/// Read the site-wide DPU UEFI password at a specific version, or `None` if no
/// secret is staged for that version.
async fn read_dpu_uefi_password(
    reader: &dyn CredentialReader,
    version: u32,
) -> Result<Option<String>, StateHandlerError> {
    let key = CredentialKey::dpu_uefi_site_default(version);
    let credentials = reader.get_credentials(&key).await.map_err(|e| {
        StateHandlerError::GenericError(eyre::eyre!(
            "read site dpu UEFI credential {}: {e}",
            key.to_key_str()
        ))
    })?;
    Ok(credentials.map(|Credentials::UsernamePassword { password, .. }| password))
}

/// Read the DPU hardware factory-default UEFI password (a hardware constant, not
/// a versioned/site credential), falling back to the well-known "bluefield"
/// default when the store has no entry -- matching the ingestion `uefi_setup`
/// path so a never-set DPU authenticates identically whether it converges via
/// ingestion or via a forced rotation.
async fn read_dpu_factory_default(
    reader: &dyn CredentialReader,
) -> Result<String, StateHandlerError> {
    let key = CredentialKey::DpuUefi {
        credential_type: CredentialType::DpuHardwareDefault {
            model: DpuModel::Unknown,
        },
    };
    let credentials = reader.get_credentials(&key).await.map_err(|e| {
        StateHandlerError::GenericError(eyre::eyre!(
            "read dpu UEFI factory default {}: {e}",
            key.to_key_str()
        ))
    })?;
    Ok(credentials
        .map(|Credentials::UsernamePassword { password, .. }| password)
        .unwrap_or_else(|| "bluefield".to_string()))
}

/// Converge one DPU's UEFI (BIOS setup) password to the staged site-wide
/// `dpu_uefi` target in `ManagedHostState::RotatingDpuUefi`, then return to
/// `Ready`. The Ready entry guard already picked `dpu_machine_id` as the DPU to
/// converge this cycle (force-requested, or lagging with rotation enabled).
///
/// Single-tick, fire-and-record (matching the DPU ingestion path): stage the
/// target through the DPU's `Bios/Settings` (authenticating with the
/// current-version credential via [`dpu_uefi_current_candidates`], not the
/// empty/factory assumption), issue the DPU restart that commits it, then record
/// convergence. Crash-safe and idempotent -- `rotating_to_version` is staged
/// before the restart and re-running just re-applies the same target and
/// re-issues the restart. A device-level failure is quarantined with exponential
/// backoff and returns to `Ready`, so the DPU never wedges the host in this
/// state; the passive gate then skips the DPU until the window elapses. A
/// missing DPU (gone from the snapshot) or missing BMC MAC returns to `Ready`
/// without acting.
pub(crate) async fn handle_rotating_dpu_uefi(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    state: &ManagedHostStateSnapshot,
    dpu_machine_id: DpuMachineId,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    use db::credential_rotation::CredentialRotationType::DpuUefi;

    let db_pool = ctx.services.db_pool.clone();

    // The entry guard selected this DPU from the same snapshot; if it is somehow
    // gone now, there is nothing to converge -- return to Ready rather than error.
    let Some(dpu) = state.dpu_snapshots.iter().find(|d| d.id == dpu_machine_id) else {
        tracing::warn!(
            %dpu_machine_id,
            "RotatingDpuUefi selected a DPU no longer present on the host; returning to Ready"
        );
        return Ok(StateHandlerOutcome::transition(ManagedHostState::Ready));
    };

    // A known DPU BMC MAC keys the dpu_uefi rotation bookkeeping; without it the
    // device can be neither tracked nor reached (the entry guard likewise never
    // selects such a DPU).
    let dpu_bmc_mac = dpu
        .status
        .bmc_info
        .mac
        .ok_or(StateHandlerError::MissingData {
            object_id: dpu.id.to_string(),
            missing: "bmc_mac",
        })?;
    let forced = dpu.uefi_credential_rotation_requested;
    let dpf_used_for_ingestion = state.host_snapshot.config.dpf.used_for_ingestion;

    let target = current_site_uefi_target(&db_pool, DpuUefi).await?;

    // The device's tracked current version selects the first authentication
    // candidate; its prior attempt count sizes the backoff on failure.
    let (current_version, prior_attempts) = {
        let mut conn = db_pool.acquire().await?;
        match db::credential_rotation::device_rotation_status(&mut conn, DpuUefi, dpu_bmc_mac)
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("read dpu uefi rotation status: {e}"))
            })? {
            Some(status) => (
                status.current_version.and_then(|v| u32::try_from(v).ok()),
                status.rotate_attempts,
            ),
            None => (None, 0),
        }
    };

    // Resolve the ordered current-password candidates and the new (target)
    // password before touching the device; scope the credential reader so it is
    // not held across the mutable-context DPU restart below.
    let (candidates, new_password) = {
        let reader = ctx.services.bmc_credential_ops.credential_reader();
        let candidates = dpu_uefi_current_candidates(reader, current_version, target).await?;
        let Credentials::UsernamePassword {
            password: new_password,
            ..
        } = resolve_site_uefi_credentials(&db_pool, reader, DpuUefi).await?;
        (candidates, new_password)
    };

    // Stage the target before dispatch (crash-safe), in its own short
    // transaction so no lock is held across the Redfish round-trip.
    {
        let mut conn = db_pool.acquire().await?;
        db::credential_rotation::mark_device_rotating_to_version(
            &mut conn,
            dpu_bmc_mac,
            DpuUefi,
            target as i32,
        )
        .await
        .map_err(|e| {
            StateHandlerError::GenericError(eyre!("stage dpu uefi rotating_to_version: {e}"))
        })?;
    }

    let access = ctx.services.bmc_access_info_for_machine(dpu).await?;
    match ctx
        .services
        .bmc_credential_ops
        .rotate_uefi_password(&access, &candidates, new_password)
        .await
    {
        // The DPU stages the change through Bios/Settings and schedules no job
        // (`job_id` is always `None`); the restart below commits it.
        Ok(_job_id) => {
            handler_restart_dpu(dpu, ctx, dpf_used_for_ingestion).await?;

            let mut txn = db_pool.begin().await?;
            let promoted = db::credential_rotation::promote_rotating_to_current(
                &mut txn,
                dpu_bmc_mac,
                DpuUefi,
            )
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("promote dpu uefi rotating_to_version: {e}"))
            })?;
            if !promoted {
                db::credential_rotation::record_device_converged(&mut txn, dpu_bmc_mac, DpuUefi)
                    .await
                    .map_err(|e| {
                        StateHandlerError::GenericError(eyre!("record dpu uefi convergence: {e}"))
                    })?;
            }
            tracing::info!(mac = %dpu_bmc_mac, %dpu_machine_id, "DPU UEFI converged to site-wide rotation target");
            // A forced attempt genuinely fired, so clear the one-shot request on
            // the same transaction; a re-force is a fresh operator action.
            if forced {
                db::machine::clear_uefi_credential_rotation_requested(
                    &mut txn,
                    dpu_machine_id.into(),
                )
                .await?;
            }
            Ok(StateHandlerOutcome::transition(ManagedHostState::Ready).with_txn(txn))
        }
        // Client creation failed (credential store, TCP, or the vendor
        // probe): return Err so the state framework retries; quarantine is
        // reserved for the operation itself failing on the device.
        Err(CredentialOpError::ClientCreation(e)) => Err(e.into()),
        Err(CredentialOpError::Operation(e)) => {
            // Device-level failure (all current-password candidates rejected, or
            // the DPU refused the change). The pool already redacted the password
            // out of the error. Quarantine with backoff and return to Ready so
            // the host never wedges in this state.
            let redacted = e.to_string();
            let quarantined_until =
                db::credential_rotation::backoff_until(prior_attempts, chrono::Utc::now());
            let mut txn = db_pool.begin().await?;
            db::credential_rotation::increment_rotate_attempt(
                &mut txn,
                dpu_bmc_mac,
                DpuUefi,
                &redacted,
                quarantined_until,
            )
            .await
            .map_err(|e| {
                StateHandlerError::GenericError(eyre!("record dpu uefi rotation failure: {e}"))
            })?;
            tracing::warn!(
                mac = %dpu_bmc_mac,
                %dpu_machine_id,
                %quarantined_until,
                error = %redacted,
                "DPU UEFI rotation attempt failed; quarantined until backoff elapses"
            );
            // A forced attempt genuinely fired, so clear the one-shot request on
            // the same transaction; a re-force is a fresh operator action.
            if forced {
                db::machine::clear_uefi_credential_rotation_requested(
                    &mut txn,
                    dpu_machine_id.into(),
                )
                .await?;
            }
            Ok(StateHandlerOutcome::transition(ManagedHostState::Ready).with_txn(txn))
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_secrets::MemoryCredentialStore;
    use carbide_secrets::credentials::CredentialWriter;

    use super::*;

    /// Seed a versioned DPU UEFI secret into an in-memory reader.
    async fn seed_version(store: &MemoryCredentialStore, version: u32, password: &str) {
        store
            .set_credentials(
                &CredentialKey::dpu_uefi_site_default(version),
                &Credentials::UsernamePassword {
                    username: String::new(),
                    password: password.to_string(),
                },
            )
            .await
            .expect("seeding a dpu UEFI secret should succeed");
    }

    /// Seed the DPU hardware factory default into an in-memory reader.
    async fn seed_factory_default(store: &MemoryCredentialStore, password: &str) {
        store
            .set_credentials(
                &CredentialKey::DpuUefi {
                    credential_type: CredentialType::DpuHardwareDefault {
                        model: DpuModel::Unknown,
                    },
                },
                &Credentials::UsernamePassword {
                    username: String::new(),
                    password: password.to_string(),
                },
            )
            .await
            .expect("seeding the dpu UEFI factory default should succeed");
    }

    /// A tracked DPU lists its current-version secret first, then the target
    /// version, then the factory default, so authentication tries the most
    /// likely current password before falling back.
    #[tokio::test]
    async fn candidates_are_current_then_target_then_factory_default() {
        let store = MemoryCredentialStore::default();
        seed_version(&store, 1, "current-v1").await;
        seed_version(&store, 2, "target-v2").await;
        seed_factory_default(&store, "bf-default").await;

        let candidates = dpu_uefi_current_candidates(&store, Some(1), 2)
            .await
            .expect("resolving candidates should succeed");

        assert_eq!(
            candidates,
            vec![
                "current-v1".to_string(),
                "target-v2".to_string(),
                "bf-default".to_string(),
            ],
        );
    }

    /// A never-rotated DPU (no tracked current version) tries the target then
    /// the factory default -- the first-rotation versioned-vs-factory case.
    #[tokio::test]
    async fn candidates_for_a_never_rotated_dpu_are_target_then_factory_default() {
        let store = MemoryCredentialStore::default();
        seed_version(&store, 0, "legacy-v0").await;
        seed_factory_default(&store, "bf-default").await;

        let candidates = dpu_uefi_current_candidates(&store, None, 0)
            .await
            .expect("resolving candidates should succeed");

        assert_eq!(
            candidates,
            vec!["legacy-v0".to_string(), "bf-default".to_string()],
        );
    }

    /// When current and target are the same version it is listed once, so we
    /// never probe the same password twice.
    #[tokio::test]
    async fn candidates_dedupe_when_current_equals_target() {
        let store = MemoryCredentialStore::default();
        seed_version(&store, 3, "v3").await;
        seed_factory_default(&store, "bf-default").await;

        let candidates = dpu_uefi_current_candidates(&store, Some(3), 3)
            .await
            .expect("resolving candidates should succeed");

        assert_eq!(candidates, vec!["v3".to_string(), "bf-default".to_string()],);
    }

    /// With no factory-default secret staged, the walk falls back to the
    /// well-known "bluefield" hardware default so a never-set DPU still has a
    /// terminal candidate.
    #[tokio::test]
    async fn factory_default_falls_back_to_bluefield() {
        let store = MemoryCredentialStore::default();
        seed_version(&store, 1, "v1").await;

        let candidates = dpu_uefi_current_candidates(&store, Some(1), 1)
            .await
            .expect("resolving candidates should succeed");

        assert_eq!(candidates, vec!["v1".to_string(), "bluefield".to_string()]);
    }
}
