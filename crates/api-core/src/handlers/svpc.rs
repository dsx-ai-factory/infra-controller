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

use std::borrow::Cow;

use ::rpc::protos::mlx_device as mlx_device_pb;
use carbide_host_support::dpa_cmds::{DpaCommand, DpaDeviceCommand, OpCode};
use carbide_uuid::machine::HostMachineId;
use db::dpa_interface;
use eyre::eyre;
use libmlx::device::report::MlxDeviceReport;
use libmlx::profile::serialization::SerializableProfile;
use mac_address::MacAddress;
use model::dpa_interface::{
    CardState, DpaInterface, DpaInterfaceControllerState, DpaInterfaceType, DpaLockMode,
    DpaSearchConfig, NewDpaInterface,
};
use rpc::forge_agent_control_response as fac;
use rpc::forge_agent_control_response::MlxDeviceAction;
use rpc::protos::mlx_device::MlxDeviceInfo;
use tonic::{Request, Response, Status};

use crate::api::{Api, log_request_data};
use crate::machine_update_manager::metrics::{
    FirmwareUpdatePhase, FirmwareUpdateProgress, FirmwareUpdateTarget,
};
use crate::{CarbideError, CarbideResult};

// Code to handle SVPC specific information.

/// Process a request from the Scout. The Scout periodically queries Carbide to determine
/// what it should do (ForgeAgentControlRequest). We found the machine in DpaProvisioning state.
/// So look at each DPA interface and make it progress through the state machine.
/// If there is work to be done, return an MLX action with per-device commands.
pub(super) async fn process_scout_req(
    api: &Api,
    machine_id: HostMachineId,
) -> CarbideResult<fac::Action> {
    if !api.runtime_config.is_ewethers_enabled() || !api.runtime_config.is_svpc_enabled() {
        tracing::info!(
            "DPA is not enabled or SVPC is not enabled, skipping SVPC process_scout_req"
        );
        return Ok(fac::Action::noop());
    }

    let dpa_search_config = DpaSearchConfig {
        only_svpc: true,
        only_astra: false,
    };

    let dpa_snapshots = db::dpa_interface::find_by_machine_id(
        &api.database_connection,
        machine_id,
        dpa_search_config,
    )
    .await?;

    if dpa_snapshots.is_empty() {
        tracing::error!(
            %machine_id,
            "No DPA snapshots found",
        );
        return Ok(fac::Action::noop());
    }

    // Whether a tenant-allocation lock this pass should migrate the card to the
    // site-wide target. It is host-scoped (the same for every card) and needed
    // only if some card actually reaches `Locking`, so it is resolved lazily and
    // cached here -- at most one read per pass, and none when no card locks.
    let mut rotate_lockdown_key: Option<bool> = None;

    let mut device_actions = Vec::new();

    for sn in &dpa_snapshots {
        let cstate = &sn.controller_state.value;
        let pci_name = &sn.pci_name;

        if sn.interface_type != DpaInterfaceType::Svpc {
            tracing::error!(
                %machine_id, %pci_name,
                "interface type is not Svpc, skipping"
            );
            continue;
        }

        let dpa_cmd = match cstate {
            DpaInterfaceControllerState::Provisioning
            | DpaInterfaceControllerState::Ready
            | DpaInterfaceControllerState::Assigned => continue, // We are in the Assigned state, so we don't need to do anything

            DpaInterfaceControllerState::Unlocking => {
                build_unlock_command(api, sn, machine_id, pci_name).await?
            }
            DpaInterfaceControllerState::ApplyFirmware => {
                build_apply_firmware_command(api, sn, machine_id, pci_name)
            }
            DpaInterfaceControllerState::ApplyProfile => {
                build_apply_profile_command(api, sn, machine_id, pci_name)?
            }
            DpaInterfaceControllerState::Locking => {
                // Tenant-allocation lock: migrate to the site-wide target when the
                // gate is on or this host is force-flagged, otherwise re-lock at
                // the card's current version. Resolve+cache the host-scoped
                // decision on the first `Locking` card of the pass.
                let rotate = match rotate_lockdown_key {
                    Some(v) => v,
                    None => {
                        let v = resolve_rotate_lockdown_key(api, machine_id).await?;
                        rotate_lockdown_key = Some(v);
                        v
                    }
                };
                build_lock_command(api, sn, machine_id, pci_name, rotate).await?
            }
            DpaInterfaceControllerState::RotateKeyUnlocking => {
                // Tenant-free lockdown rotation in progress: unlock nic
                build_unlock_command(api, sn, machine_id, pci_name).await?
            }
            DpaInterfaceControllerState::RotateKeyLocking => {
                // Tenant-free lockdown rotation in progress: relock at the
                // site-wide target unconditionally. Entering this state already
                // committed to migrating (the unlock phase NULLed the card's
                // current version), so this must not consult the gate.
                build_lock_command(api, sn, machine_id, pci_name, true).await?
            }
        };

        match MlxDeviceAction::try_from(DpaDeviceCommand {
            pci_name: pci_name.clone(),
            command: dpa_cmd,
        }) {
            Ok(action) => device_actions.push(action),
            Err(e) => {
                // Would only happen if the op is an ApplyProfile command with invalid YAML
                tracing::error!(
                    error = %e,
                    "Failed to encode DPA command",
                );
            }
        }
    }

    Ok(fac::Action::MlxAction(fac::MlxAction { device_actions }))
}

/// Decide whether a tenant-allocation lock should migrate the host's cards to
/// the site-wide lockdown IKM target this pass.
///
/// The site-wide `nic_lockdown_ikm_rotation_enabled` gate is the lazy-migration
/// switch: when on, cards migrate to the new IKM as tenants cycle. The per-host
/// `lockdown_ikm_credential_rotation_requested` force flag overrides it so an
/// operator can converge a single host while the site-wide gate is still off.
///
/// The gate is checked first so the host-scoped DB read is skipped whenever the
/// gate is on (the answer is already `true`); the force flag is only consulted
/// when the gate is off.
async fn resolve_rotate_lockdown_key(api: &Api, machine_id: HostMachineId) -> CarbideResult<bool> {
    if api.runtime_config.nic_lockdown_ikm_rotation_enabled {
        return Ok(true);
    }
    let mut conn = api.database_connection.acquire().await.map_err(|e| {
        CarbideError::GenericErrorFromReport(eyre!(
            "failed to acquire connection to read nic lockdown force flag: {e}"
        ))
    })?;
    Ok(db::machine::get_lockdown_ikm_credential_rotation_requested(&mut conn, machine_id).await?)
}

/// Resolve the lockdown IKM version to *lock* a card under.
///
/// An in-flight lock takes precedence over either branch below: if a lock is
/// already staged (`rotating_to_version` set), we re-derive that exact version.
/// A card's physical lock version is frozen once it locks -- it cannot change
/// without an unlock first -- and dpa-manager's `handle_locking` promotes
/// whatever `rotating_to_version` holds when the card reports Locked (it checks
/// lockmode, never the version). Re-reading the site-wide target or the card's
/// last-confirmed `current_version` on a later reconciliation could overwrite the
/// staged marker and record a convergence version the hardware was never on --
/// e.g. if the migration decision flips between staging the lock and observing
/// it. This mirrors the unlock path, which already prefers `rotating_to_version`.
///
/// When no lock is in flight, the version is chosen by `migrate_to_target`. The
/// caller owns that decision, so this resolver is shared by both lock paths:
///
/// - The tenant-allocation `Locking` passes `force || nic_lockdown_ikm_rotation_enabled`.
///   The site-wide flag is the lazy-migration gate (cards migrate to the new IKM
///   as tenants cycle); the per-host force flag overrides it so an operator can
///   converge one host even while the site-wide gate is off.
/// - The idle `RotateKeyLocking` passes `true` unconditionally: entering that
///   state is itself the commitment to migrate (the unlock phase already NULLed
///   `current_version`), so it must resolve to the target regardless of the flag
///   -- otherwise a mid-rekey kill-switch flip would fall through to the seed and
///   silently downgrade the card.
///
/// When `migrate_to_target` is false, the version is the card's own current
/// tracked version, so a staged `RotateCredential(lockdown_ikm)` target does not
/// migrate any card until the deliberate cutover flip, and an already-migrated
/// card re-locks at the version it is on rather than being reverted.
///
/// The two branches resolve their fallback differently on purpose. The site-wide
/// `lockdown_ikm` target row is seeded for every site by the backfill migration
/// (and by `set_initial_target_version` on the first `RotateCredential`), so a
/// missing target is a corrupted invariant we surface rather than paper over --
/// mirroring `record_device_converged`. A per-card row, by contrast, is created
/// lazily on first lock and only backfilled for already-locked cards, so a card
/// with no row / no tracked version legitimately falls back to the seed version.
async fn resolve_lock_ikm_version(
    api: &Api,
    mac: MacAddress,
    migrate_to_target: bool,
) -> CarbideResult<i32> {
    let mut conn = api.database_connection.acquire().await.map_err(|e| {
        CarbideError::GenericErrorFromReport(eyre!(
            "failed to acquire connection to resolve lockdown lock IKM version: {e}"
        ))
    })?;
    // An in-flight lock wins over both branches: re-derive the staged version so
    // a later reconciliation cannot overwrite it (see the doc comment).
    let device_state = db::credential_rotation::device_rotation_operation_state(
        &mut *conn,
        db::credential_rotation::CredentialRotationType::LockdownIkm,
        mac,
    )
    .await?;
    if let Some(in_flight) = device_state.as_ref().and_then(|s| s.rotating_to_version) {
        return Ok(in_flight);
    }

    // Versions stay DB-native `i32` throughout this handler (Postgres has no
    // unsigned int); the sole `u32` conversion happens once at the derivation
    // boundary (`ikm_version_u32` -> `build_supernic_lockdown_key`).
    let version = if migrate_to_target {
        db::credential_rotation::current_target_version(
            &mut conn,
            db::credential_rotation::CredentialRotationType::LockdownIkm,
        )
        .await?
        .ok_or(db::DatabaseError::MissingSitewideRotationTarget(
            db::credential_rotation::CredentialRotationType::LockdownIkm,
        ))?
    } else {
        device_state
            .and_then(|s| s.current_version)
            .unwrap_or(crate::dpa::lockdown::SEED_LOCKDOWN_IKM_VERSION as i32)
    };
    Ok(version)
}

/// Resolve the lockdown IKM version to *unlock* a card with: the version it is
/// actually locked under. That is the in-flight `rotating_to_version` if a lock
/// is mid-flight (a crash window where the card may already be physically locked
/// under it), otherwise the last-confirmed `current_version`, otherwise the seed
/// version for a card with no tracked lock yet.
///
/// This is always version-aware, independent of `nic_lockdown_ikm_rotation_enabled`: a
/// card that already migrated to a newer IKM must be unlocked with that IKM, or
/// it would be bricked. Because the assignment cycle always unlocks a card
/// before re-locking it, a locked card's version is always exactly this resolved
/// value, so a single key suffices.
async fn resolve_unlock_ikm_version(api: &Api, mac: MacAddress) -> CarbideResult<i32> {
    let mut conn = api.database_connection.acquire().await.map_err(|e| {
        CarbideError::GenericErrorFromReport(eyre!(
            "failed to acquire connection to resolve lockdown unlock IKM version: {e}"
        ))
    })?;
    // DB-native `i32`; converted to `u32` once via `ikm_version_u32` at the
    // derivation call. No row / no tracked version falls back to the seed.
    let version = db::credential_rotation::device_rotation_operation_state(
        &mut *conn,
        db::credential_rotation::CredentialRotationType::LockdownIkm,
        mac,
    )
    .await?
    .and_then(|s| s.rotating_to_version.or(s.current_version))
    .unwrap_or(crate::dpa::lockdown::SEED_LOCKDOWN_IKM_VERSION as i32);
    Ok(version)
}

/// Build and return a command to unlock the DPA.
async fn build_unlock_command(
    api: &Api,
    sn: &DpaInterface,
    machine_id: HostMachineId,
    pci_name: &str,
) -> CarbideResult<DpaCommand<'static>> {
    // DB-native `i32`; converted to the `u32` the derivation layer uses. The
    // rotation columns carry a non-negative CHECK, so a negative here is a
    // corrupted invariant, surfaced rather than silently wrapped.
    let version = resolve_unlock_ikm_version(api, sn.mac_address).await?;
    let ikm_version = u32::try_from(version).map_err(|e| CarbideError::Internal {
        message: format!(
            "lockdown IKM unlock version {version} is negative for DPA {pci_name}: {e}"
        ),
    })?;

    let key = crate::dpa::lockdown::build_supernic_lockdown_key(
        &api.database_connection,
        sn.id,
        &*api.credential_manager,
        ikm_version,
    )
    .await
    .map_err(|e| {
        CarbideError::GenericErrorFromReport(eyre!(
            "failed to build unlock key for DPA {pci_name}: {e}"
        ))
    })?;

    tracing::info!(%machine_id, %pci_name, ikm_version, "Unlocking DPA");

    // The unlock flow does not record convergence, so the derived IKM version is
    // not persisted here.
    Ok(DpaCommand {
        op: OpCode::Unlock { key },
    })
}

/// Build and return a command to apply firmware to the DPA.
fn build_apply_firmware_command<'a>(
    api: &'a Api,
    sn: &DpaInterface,
    machine_id: HostMachineId,
    pci_name: &str,
) -> DpaCommand<'a> {
    // Look up a FirmwareFlasherProfile for the device's PN:PSID
    // from the runtime config. If a profile exists and the device
    // is already at the target version, skip. Otherwise pass the
    // profile down to scout.
    let profile = (|| {
        let Some(device_info) = &sn.device_info else {
            tracing::warn!(
                %machine_id, %pci_name,
                "no device_info available, skipping firmware application"
            );
            return None;
        };

        let (Some(part_number), Some(psid)) = (&device_info.part_number, &device_info.psid) else {
            tracing::warn!(
                %machine_id, %pci_name,
                "device_info missing part_number and/or psid, skipping firmware"
            );
            return None;
        };

        let Some(fw_profile) = api
            .runtime_config
            .get_supernic_firmware_profile(part_number, psid)
        else {
            tracing::info!(
                %machine_id, %pci_name, %part_number, %psid,
                "no firmware profile found, skipping"
            );
            return None;
        };

        if device_info.fw_version_current.as_deref()
            == Some(fw_profile.firmware_spec.version.as_str())
        {
            tracing::info!(
                %machine_id, %pci_name, %part_number, %psid,
                observed_firmware_version = ?device_info.fw_version_current,
                expected_firmware_version = %fw_profile.firmware_spec.version,
                "firmware already at target version, skipping"
            );
            return None;
        }

        carbide_instrument::emit(FirmwareUpdateProgress {
            target: FirmwareUpdateTarget::SuperNic,
            phase: FirmwareUpdatePhase::Started,
            machine_id: machine_id.into(),
            detail: format!(
                "pci_name={pci_name} part_number={part_number} psid={psid} \
                 observed_fw_version={:?} expected_fw_version={}",
                device_info.fw_version_current, fw_profile.firmware_spec.version
            ),
        });
        Some(Cow::Borrowed(fw_profile))
    })();

    tracing::info!(%machine_id, %pci_name, "ApplyFirmware");
    DpaCommand {
        op: OpCode::ApplyFirmware {
            profile: profile.map(Box::new),
        },
    }
}

// build_apply_profile_command takes a target DpaInterface
// and looks to see if an mlxconfig_profile name has been
// configured for it. If not, then we'll return None, which
// will make its way to scout, signaling that it just needs
// to do a simple reset of mlxconfig parameters. If a name
// HAS been set, then we will attempt to look it up in the
// runtime config, and then serialize the values to populate
// in the DpaCommand and send them down to the device.
//
// If a profile name is configured but cannot be resolved or
// serialized, this returns an error — we must not send a None
// to scout, as that would reset the card to factory defaults
// without applying the intended profile.
/// Build and return a command to apply a profile to the DPA.
fn build_apply_profile_command(
    api: &Api,
    interface: &DpaInterface,
    machine_id: HostMachineId,
    pci_name: &str,
) -> CarbideResult<DpaCommand<'static>> {
    let Some(profile_name) = &interface.mlxconfig_profile else {
        tracing::info!(
            %machine_id, %pci_name,
            "no mlxconfig_profile assigned, reset only"
        );
        return Ok(DpaCommand {
            op: OpCode::ApplyProfile {
                serialized_profile: None,
            },
        });
    };

    let mlxconfig_profile = api
        .runtime_config
        .get_mlxconfig_profile(profile_name)
        .ok_or_else(|| {
            tracing::error!(
                %machine_id, %pci_name, %profile_name,
                "mlxconfig_profile not found in config"
            );
            CarbideError::NotFoundError {
                kind: "mlxconfig_profile",
                id: profile_name.clone(),
            }
        })?;

    let serialized_profile = SerializableProfile::from_profile(mlxconfig_profile).map_err(|e| {
        tracing::error!(
            %machine_id, %pci_name, %profile_name,
            error = %e,
            "failed to serialize mlxconfig profile"
        );
        CarbideError::Internal {
            message: format!("failed to serialize mlxconfig_profile '{profile_name}': {e}"),
        }
    })?;

    tracing::info!(%machine_id, %pci_name, %profile_name, "ApplyProfile");

    Ok(DpaCommand {
        op: OpCode::ApplyProfile {
            serialized_profile: Some(serialized_profile),
        },
    })
}

/// Build and return a command to lock the DPA.
///
/// `migrate_to_target` selects the lock version via [`resolve_lock_ikm_version`]:
/// the tenant-allocation `Locking` passes `force || nic_lockdown_ikm_rotation_enabled`
/// (lazy migration, overridable per host), while the idle `RotateKeyLocking`
/// passes `true` because entering that state already committed to converging on
/// the site-wide target.
async fn build_lock_command(
    api: &Api,
    sn: &DpaInterface,
    machine_id: HostMachineId,
    pci_name: &str,
    migrate_to_target: bool,
) -> CarbideResult<DpaCommand<'static>> {
    let target_version = resolve_lock_ikm_version(api, sn.mac_address, migrate_to_target).await?;
    lock_command_for_target(api, sn, machine_id, pci_name, target_version).await
}

/// Shared lock builder: derive the lock key at `target_version`, stage it as the
/// in-flight `rotating_to_version` marker, and return the `OpCode::Lock`.
async fn lock_command_for_target(
    api: &Api,
    sn: &DpaInterface,
    machine_id: HostMachineId,
    pci_name: &str,
    target_version: i32,
) -> CarbideResult<DpaCommand<'static>> {
    let ikm_version = u32::try_from(target_version).map_err(|e| CarbideError::Internal {
        message: format!(
            "lockdown IKM lock version {target_version} is negative for DPA {pci_name}: {e}"
        ),
    })?;

    let key = crate::dpa::lockdown::build_supernic_lockdown_key(
        &api.database_connection,
        sn.id,
        &*api.credential_manager,
        ikm_version,
    )
    .await
    .map_err(|e| {
        CarbideError::GenericErrorFromReport(eyre!(
            "failed to build lock key for DPA {pci_name}: {e}"
        ))
    })?;

    // Stage the IKM version we are about to lock the card with as the in-flight
    // rotation marker (`rotating_to_version`) on the card's lockdown_ikm row
    // *before* issuing the lock command. dpa-manager's `handle_locking` promotes
    // exactly this value to the convergence version when the card reports Locked
    // -- never the (possibly advanced) site-wide target re-read at observation
    // time. Staging first means we only ever issue a lock for a version we have
    // already recorded our intent to use; if the write fails we surface the error
    // and do not lock.
    let mut conn = api.database_connection.acquire().await.map_err(|e| {
        CarbideError::GenericErrorFromReport(eyre!(
            "failed to acquire connection to stage lockdown IKM rotation for DPA {pci_name}: {e}"
        ))
    })?;
    db::credential_rotation::mark_device_rotating_to_version(
        &mut conn,
        sn.mac_address,
        db::credential_rotation::CredentialRotationType::LockdownIkm,
        target_version,
    )
    .await?;

    tracing::info!(%machine_id, %pci_name, ikm_version = target_version, "Locking DPA");
    Ok(DpaCommand {
        op: OpCode::Lock { key },
    })
}

/// The scout is sending us an mlx observation report. The report will
/// consist of a vector of observations, one for each mlx device.
/// Based on what is being reported, we update the card_state of the
/// corresponding DB entry. This update is noticed by the DPA statecontroller
/// and will cause it to advance to the next state.
async fn process_mlx_observation(
    api: &Api,
    request: tonic::Request<mlx_device_pb::PublishMlxObservationReportRequest>,
) -> CarbideResult<()> {
    // Prepare our txn to grab the dpa interfaces from the DB
    let mut txn = api.txn_begin().await?;

    let req = request.into_inner();

    let Some(rep) = req.report else {
        tracing::error!("MLX observation request is missing its report");
        return Err(CarbideError::GenericErrorFromReport(eyre!(
            "process_mlx_observation without report req: {:#?}",
            req
        )));
    };

    let Some(machine_id) = rep.machine_id else {
        tracing::error!(
            observation_count = rep.observations.len(),
            "MLX device report is missing its machine ID",
        );
        return Err(CarbideError::GenericErrorFromReport(eyre!(
            "process_mlx_observation without machine_id report: {:#?}",
            rep
        )));
    };

    let dpa_search_config = DpaSearchConfig {
        only_svpc: true,
        only_astra: false,
    };

    let dpa_snapshots = db::dpa_interface::find_by_machine_id(
        &mut txn,
        machine_id.try_into().map_err(|error| {
            CarbideError::InvalidArgument(format!("invalid host machine ID: {error}"))
        })?,
        dpa_search_config,
    )
    .await?;

    if dpa_snapshots.is_empty() {
        tracing::error!(
            %machine_id,
            "No DPA snapshots found",
        );
        return Err(CarbideError::GenericErrorFromReport(eyre!(
            "process_mlx_observation no dpa snapshots for machine: {:#?}",
            machine_id
        )));
    }

    if rep.observations.is_empty() {
        tracing::error!(
            %machine_id,
            observation_count = rep.observations.len(),
            "MLX device report contains no observations",
        );
        return Err(CarbideError::GenericErrorFromReport(eyre!(
            "process_mlx_observation no observations in report: {:#?}",
            rep
        )));
    }

    for obs in rep.observations {
        let Some(devinfo) = obs.device_info else {
            tracing::error!(
                %machine_id,
                "MLX device observation contains no device info",
            );
            continue;
        };

        let mut dpa = match get_dpa_by_mac(&devinfo, &dpa_snapshots) {
            Ok(dpa) => dpa,
            Err(e) => {
                tracing::error!(
                    pci_name = %devinfo.pci_name,
                    mac_address = %devinfo.base_mac,
                    error = %e,
                    "DPA interface not found",
                );
                continue;
            }
        };

        if dpa.interface_type != DpaInterfaceType::Svpc {
            tracing::error!(
                dpa_interface_id = %dpa.id,
                %machine_id,
                pci_name = %dpa.pci_name,
                interface_type = ?dpa.interface_type,
                "DPA interface is not an SVPC interface; skipping",
            );
            continue;
        }

        // Use the latest CardState we pulled from the database. If there
        // isn't one, then initialize an empty one, for which we will now
        // update with whatever the current observation is.
        let mut cstate = dpa.card_state.unwrap_or(CardState {
            lockmode: None,
            profile: None,
            profile_synced: None,
            firmware_report: None,
        });

        if let Some(lock_status) = obs.lock_status {
            let ls = match DpaLockMode::try_from(lock_status) {
                Ok(ls) => ls,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "Failed to convert DPA lock status",
                    );
                    continue;
                }
            };

            cstate.lockmode = Some(ls);
        }

        if obs.profile_name.is_some() {
            cstate.profile = obs.profile_name;
        }

        if obs.profile_synced.is_some() {
            cstate.profile_synced = obs.profile_synced;
        }

        // If the observation contains a FirmwareFlashReport update
        // in it, then merge it into the latest CardState that we
        // pulled from the database.
        if let Some(firmware_report) = obs.firmware_report {
            cstate.firmware_report = Some(firmware_report.into());
        }

        dpa.card_state = Some(cstate);

        match dpa_interface::update_card_state(&mut txn, dpa).await {
            Ok(_id) => (),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "Failed to update DPA card state",
                );
            }
        }
    }

    txn.commit().await?;

    Ok(())
}

/// Scout is telling Carbide the mlx device configuration in its machine
pub(crate) async fn publish_mlx_device_report(
    api: &Api,
    request: Request<mlx_device_pb::PublishMlxDeviceReportRequest>,
) -> Result<Response<mlx_device_pb::PublishMlxDeviceReportResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();

    if !api.runtime_config.is_ewethers_enabled() || !api.runtime_config.is_svpc_enabled() {
        tracing::info!(
            "DPA is not enabled or SVPC is not enabled, skipping SVPC publish_mlx_device_report"
        );
        return Ok(Response::new(
            mlx_device_pb::PublishMlxDeviceReportResponse {},
        ));
    }

    if let Some(report_pb) = req.report {
        let report: MlxDeviceReport = report_pb
            .try_into()
            .map_err(|e: String| CarbideError::Internal { message: e })?;
        tracing::info!(
            hostname = %report.hostname,
            device_count = report.devices.len(),
            "received MlxDeviceReport",
        );

        // Without a machine_id, we can't create dpa interfaces
        if let Some(machine_id) = report.machine_id {
            let mut spx_nics: i32 = 0;

            // Go over each of the MlxDeviceInfo reports from the
            // MlxDeviceReport. Each MlxDeviceInfo corresponds to
            // an individual device reported by `mlxfwmanager`, with
            // the MlxDeviceReport being a report of all devices
            // reporting on a given machine.
            for device_info in report.devices {
                // XXX TODO XXX
                // Change this to base device detection using part numbers rather
                // than device description.
                // XXX TODO XXX
                let is_supernic = device_info
                    .device_description
                    .as_deref()
                    .is_some_and(|d| d.contains("SuperNIC"));
                if !is_supernic {
                    continue;
                }
                spx_nics += 1;

                let device_type = device_info.device_type.clone();
                let pci_name = device_info.pci_name.clone();
                let device_description = device_info.device_description.clone();

                let Some(new_interface) = NewDpaInterface::from_device_info(
                    machine_id.try_into().map_err(|error| {
                        CarbideError::InvalidArgument(format!("invalid host machine ID: {error}"))
                    })?,
                    device_info.base_mac,
                    device_type,
                    pci_name.clone(),
                    device_description,
                    DpaInterfaceType::Svpc,
                ) else {
                    tracing::warn!(
                        %machine_id,
                        pci_name = %pci_name,
                        "skipping interface: missing base_mac"
                    );
                    continue;
                };

                let ensured_interface =
                    match crate::handlers::dpa::ensure_interface(api, new_interface).await {
                        Ok(ensured) => {
                            tracing::info!(
                                dpa_interface_id = %ensured.id,
                                machine_id = %ensured.machine_id,
                                pci_name = %ensured.pci_name,
                                mac_address = %ensured.mac_address,
                                "ensured dpa interface exists"
                            );
                            ensured
                        }
                        Err(e) => {
                            tracing::warn!(
                                %machine_id,
                                %device_info.pci_name,
                                error = %e,
                                "failed to ensure dpa interface"
                            );
                            continue;
                        }
                    };

                // Update the MlxDeviceInfo for this device on every
                // publish_mlx_device_report call so the latest hardware
                // state is always available.
                let mut txn = match api.txn_begin().await {
                    Ok(txn) => txn,
                    Err(e) => {
                        tracing::warn!(
                            mac_address = %ensured_interface.mac_address,
                            pci_name = %ensured_interface.pci_name,
                            error = %e,
                            "failed to begin txn for device info update"
                        );
                        continue;
                    }
                };

                match dpa_interface::update_device_info(
                    txn.as_mut(),
                    ensured_interface.machine_id,
                    &ensured_interface.pci_name,
                    &device_info,
                )
                .await
                {
                    Ok(()) => {
                        if let Err(e) = txn.commit().await {
                            tracing::warn!(
                                mac_address = %ensured_interface.mac_address,
                                pci_name = %ensured_interface.pci_name,
                                error = %e,
                                "failed to commit device info update"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            mac_address = %ensured_interface.mac_address,
                            pci_name = %ensured_interface.pci_name,
                            error = %e,
                            "failed to update device info"
                        );
                    }
                }
            }

            tracing::info!(
                spx_nic_count = spx_nics,
                %machine_id,
                "counted SPX NICs",
            );
        } else {
            tracing::warn!(
                hostname = %report.hostname,
                device_count = report.devices.len(),
                "MLX device report is missing its machine ID",
            );
        }
    } else {
        tracing::warn!("no embedded MlxDeviceReport published");
    }

    Ok(Response::new(
        mlx_device_pb::PublishMlxDeviceReportResponse {},
    ))
}

/// Scout is telling carbide the observed status (locking status, card mode) of the
/// mlx devices in its host
pub(crate) async fn publish_mlx_observation_report(
    api: &Api,
    request: Request<mlx_device_pb::PublishMlxObservationReportRequest>,
) -> Result<Response<mlx_device_pb::PublishMlxObservationReportResponse>, Status> {
    log_request_data(&request);

    if !api.runtime_config.is_ewethers_enabled() || !api.runtime_config.is_svpc_enabled() {
        tracing::info!(
            "DPA is not enabled or SVPC is not enabled, skipping SVPC publish_mlx_observation_report"
        );
        return Ok(Response::new(
            mlx_device_pb::PublishMlxObservationReportResponse {},
        ));
    }

    process_mlx_observation(api, request).await?;

    Ok(Response::new(
        mlx_device_pb::PublishMlxObservationReportResponse {},
    ))
}

/// Find the DPA object in the given slice of DPA objects which matches the MAC
/// address in the device info. Linear search is fine because the slice is
/// expected to contain fewer than a dozen entries.
fn get_dpa_by_mac(devinfo: &MlxDeviceInfo, dpas: &[DpaInterface]) -> CarbideResult<DpaInterface> {
    dpas.iter()
        .find(|dpa| dpa.mac_address.to_string() == devinfo.base_mac)
        .cloned()
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "mac_addr",
            id: devinfo.base_mac.to_string(),
        })
}
