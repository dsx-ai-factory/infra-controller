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
use ::rpc::forge as rpc;
use carbide_redfish::libredfish::CredentialOpError;
use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
use carbide_uuid::machine::MachineId;
use db::WithTransaction;
use futures_util::FutureExt;
use model::machine::LoadSnapshotOptions;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_machine_id, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

/// The current site-wide host UEFI target version a device should be driven to,
/// from `sitewide_credential_rotation.target_version`. A stored `target_version`
/// of 0 is the legacy unversioned site-default baseline; a *missing* row is an
/// error, since the backfill migration seeds a `host_uefi` row on every site.
/// This is the table-driven "current site-wide host UEFI credential" resolution
/// used when *setting* the password.
async fn host_uefi_target_version(conn: &mut sqlx::PgConnection) -> Result<u32, db::DatabaseError> {
    let version = db::credential_rotation::current_target_version(
        conn,
        db::credential_rotation::CredentialRotationType::HostUefi,
    )
    .await?
    .ok_or_else(|| db::DatabaseError::Internal {
        message: "no site-wide host_uefi rotation target row exists; the backfill migration \
                  seeds one for every active credential type, so a missing row indicates a \
                  broken or unmigrated database"
            .to_string(),
    })?;
    // The column is constrained non-negative, so a failed conversion means a
    // corrupt value, not "no rotation" -- surface it rather than masking it.
    u32::try_from(version).map_err(|e| db::DatabaseError::Internal {
        message: format!("host UEFI target_version {version} is out of range for u32: {e}"),
    })
}

/// The host UEFI version a device currently carries, for authenticating against
/// its existing password when clearing it. Returns the device's converged
/// `current_version` (which can lag the site target mid-rotation, once the UEFI
/// rotation engine exists), or the site target it was recorded against when
/// `current_version` is NULL.
///
/// The caller must have already confirmed the host's UEFI password is set: a
/// password-bearing host always has a `host_uefi` convergence row keyed by its
/// BMC MAC, because NICo writes that row in the same transaction that stamps
/// `bios_password_set_time` (see `set_host_uefi_password`), and the backfill
/// seeded one for every pre-existing host with a password. A missing row is
/// therefore a broken invariant -- error rather than guessing the site target
/// and authenticating with the wrong password.
async fn host_uefi_device_version(
    conn: &mut sqlx::PgConnection,
    bmc_mac: mac_address::MacAddress,
) -> Result<u32, db::DatabaseError> {
    let status = db::credential_rotation::device_rotation_status(
        &mut *conn,
        db::credential_rotation::CredentialRotationType::HostUefi,
        bmc_mac,
    )
    .await?
    .ok_or_else(|| db::DatabaseError::Internal {
        message: format!(
            "no host_uefi device_credential_rotation row for BMC MAC {bmc_mac}, but the host's \
             UEFI password is set; convergence is recorded alongside bios_password_set_time, so a \
             missing row indicates broken rotation bookkeeping or an unmigrated database"
        ),
    })?;
    let version = status.current_version.unwrap_or(status.target_version);
    u32::try_from(version).map_err(|e| db::DatabaseError::Internal {
        message: format!("host UEFI device version {version} is out of range for u32: {e}"),
    })
}

/// Read the host UEFI credential at `key`, mapping a store error or a missing
/// secret to a `CarbideError`. Used to resolve the password the low-level
/// `redfish` UEFI calls apply (they no longer read the store themselves).
///
/// Takes no database connection: the reader can be Vault-backed, so callers
/// resolve the version-to-`key` half against the DB first, commit, then call
/// this -- the remote read must not run while a connection is held.
pub(super) async fn read_uefi_credentials(
    reader: &dyn CredentialReader,
    key: &CredentialKey,
) -> Result<Credentials, CarbideError> {
    reader
        .get_credentials(key)
        .await
        .map_err(|e| {
            CarbideError::internal(format!(
                "failed to read UEFI credential {}: {e}",
                key.to_key_str()
            ))
        })?
        .ok_or_else(|| {
            CarbideError::internal(format!("UEFI credential {} is not set", key.to_key_str()))
        })
}

/// The `CredentialKey` for the site-wide host UEFI password to *set* on a
/// device: the secret at the current `host_uefi` target version (table-driven;
/// v0 = the legacy unversioned site-default).
///
/// This is the database half of resolving the credential -- it reads only the
/// version and returns the key. The caller reads the secret with
/// `read_uefi_credentials` after committing, so no connection is held across the
/// remote reader (Vault) request.
async fn host_uefi_set_credential_key(
    conn: &mut sqlx::PgConnection,
) -> Result<CredentialKey, CarbideError> {
    let version = host_uefi_target_version(conn).await.map_err(|e| {
        CarbideError::internal(format!("failed to read host UEFI target version: {e}"))
    })?;
    Ok(CredentialKey::host_uefi_site_default(version))
}

/// The `CredentialKey` for the host UEFI password a device currently holds, to
/// authenticate a *clear* against its existing password (the device's converged
/// version; see [`host_uefi_device_version`]).
///
/// Like `host_uefi_set_credential_key`, this is only the database half: it reads
/// the device version and returns the key. The caller reads the secret with
/// `read_uefi_credentials` after committing, so no connection is held across the
/// remote reader (Vault) request.
pub(super) async fn host_uefi_clear_credential_key(
    conn: &mut sqlx::PgConnection,
    bmc_mac: mac_address::MacAddress,
) -> Result<CredentialKey, CarbideError> {
    let version = host_uefi_device_version(conn, bmc_mac).await.map_err(|e| {
        CarbideError::internal(format!("failed to read host UEFI device version: {e}"))
    })?;
    Ok(CredentialKey::host_uefi_site_default(version))
}

/// The current site-wide DPU UEFI target version a device should be driven to,
/// from `sitewide_credential_rotation.target_version`. Mirrors
/// [`host_uefi_target_version`] for the `dpu_uefi` family: version 0 is the
/// legacy unversioned site-default baseline and a *missing* row is an error,
/// since the backfill migration seeds a `dpu_uefi` row on every site.
async fn dpu_uefi_target_version(conn: &mut sqlx::PgConnection) -> Result<u32, db::DatabaseError> {
    let version = db::credential_rotation::current_target_version(
        conn,
        db::credential_rotation::CredentialRotationType::DpuUefi,
    )
    .await?
    .ok_or_else(|| db::DatabaseError::Internal {
        message: "no site-wide dpu_uefi rotation target row exists; the backfill migration \
                  seeds one for every active credential type, so a missing row indicates a \
                  broken or unmigrated database"
            .to_string(),
    })?;
    u32::try_from(version).map_err(|e| db::DatabaseError::Internal {
        message: format!("dpu UEFI target_version {version} is out of range for u32: {e}"),
    })
}

/// The `CredentialKey` for the site-wide DPU UEFI password to *set* on a device:
/// the secret at the current `dpu_uefi` target version (table-driven; v0 = the
/// legacy unversioned site-default). The DPU analogue of
/// [`host_uefi_set_credential_key`]; only the database half (reads the version
/// and returns the key), so no connection is held across the remote reader.
async fn dpu_uefi_set_credential_key(
    conn: &mut sqlx::PgConnection,
) -> Result<CredentialKey, CarbideError> {
    let version = dpu_uefi_target_version(conn).await.map_err(|e| {
        CarbideError::internal(format!("failed to read dpu UEFI target version: {e}"))
    })?;
    Ok(CredentialKey::dpu_uefi_site_default(version))
}

pub(crate) async fn clear_host_uefi_password(
    api: &Api,
    request: Request<rpc::ClearHostUefiPasswordRequest>,
) -> Result<Response<rpc::ClearHostUefiPasswordResponse>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let request = request.into_inner();

    // https://github.com/NVIDIA/carbide-core/issues/116
    // Resolve machine_id from machine_query first (preferred),
    // otherwise fall back to the host_id (now deprecated).
    let machine_id = if let Some(query) = request.machine_query {
        match db::machine::find_by_query(&mut txn, &query).await? {
            Some(machine) => {
                log_machine_id(&machine.id);
                machine.id
            }
            None => {
                return Err(CarbideError::NotFoundError {
                    kind: "machine",
                    id: query,
                }
                .into());
            }
        }
    } else {
        // Old logic that used to assume machine ID only. If you
        // use anything other than a machine ID here it's going
        // to yell (e.g. old carbide-admin-cli).
        convert_and_log_machine_id(request.host_id.as_ref())?
    };

    if !machine_id.machine_type().is_host() {
        return Err(CarbideError::InvalidArgument(
            "carbide only supports clearing the UEFI password on discovered hosts".into(),
        )
        .into());
    }

    let snapshot = db::managed_host::load_snapshot(
        &mut txn,
        &machine_id,
        LoadSnapshotOptions {
            include_history: false,
            include_instance_data: false,
            host_health_config: api.runtime_config.host_health,
        },
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;

    // If we have no record of a UEFI password ever being set on this host,
    // there is nothing to clear. Issuing the redfish ChangePassword call in
    // that case fails on the BMC with a confusing 400 (the OldPassword we
    // send doesn't match the empty value the BMC expects), so short-circuit
    // with a warning and a successful no-op response instead of surfacing an
    // internal error to the operator.
    if snapshot.host_snapshot.bios_password_set_time.is_none() {
        txn.commit().await?;
        tracing::warn!(
            %machine_id,
            "No UEFI password is recorded as set on this host; nothing to clear"
        );
        return Ok(Response::new(rpc::ClearHostUefiPasswordResponse {
            job_id: None,
        }));
    }

    let addr = snapshot.host_snapshot.bmc_addr().ok_or_else(|| {
        CarbideError::InvalidArgument("specified machine does not have BMC address".into())
    })?;

    // Clearing must authenticate with the password the device currently carries,
    // which is resolved per-device by BMC MAC. Without the MAC we cannot pick the
    // right credential, so fail rather than guess the site default and present a
    // wrong password to the BMC.
    let bmc_mac = snapshot.host_snapshot.status.bmc_info.mac.ok_or_else(|| {
        CarbideError::InvalidArgument(
            "specified machine does not have a known BMC MAC address".into(),
        )
    })?;

    let bmc_access_info =
        db::machine_interface::lookup_bmc_access_info(&mut txn, addr.ip(), Some(addr.port()))
            .await?;

    // Resolve which credential the clear must authenticate with -- the password
    // the device currently holds, keyed by its converged version (table-driven;
    // see `host_uefi_clear_credential_key`). This is the DB half; do it while the
    // txn is open.
    let clear_key = host_uefi_clear_credential_key(&mut txn, bmc_mac).await?;

    // Commit before the remote reader (Vault) request so the connection is not
    // held across it, then read the actual secret.
    txn.commit().await?;
    let clear_credentials =
        read_uefi_credentials(api.bmc_credential_ops.credential_reader(), &clear_key).await?;

    let job_id: Option<String> = api
        .bmc_credential_ops
        .clear_host_uefi_password(&bmc_access_info, clear_credentials)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to run clear_host_uefi_password call");
            CarbideError::internal(format!(
                "failed redfish clear_host_uefi_password subtask: {e}"
            ))
        })?;

    Ok(Response::new(rpc::ClearHostUefiPasswordResponse { job_id }))
}

pub(crate) async fn set_host_uefi_password(
    api: &Api,
    request: Request<rpc::SetHostUefiPasswordRequest>,
) -> Result<Response<rpc::SetHostUefiPasswordResponse>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let request = request.into_inner();

    // https://github.com/NVIDIA/carbide-core/issues/116
    // Resolve machine_id from machine_query first (preferred),
    // otherwise fall back to the host_id (now deprecated).
    let machine_id = if let Some(query) = request.machine_query {
        match db::machine::find_by_query(&mut txn, &query).await? {
            Some(machine) => {
                log_machine_id(&machine.id);
                machine.id
            }
            None => {
                return Err(CarbideError::NotFoundError {
                    kind: "machine",
                    id: query,
                }
                .into());
            }
        }
    } else {
        // Old logic that used to assume machine ID only. If you
        // use anything other than a machine ID here it's going
        // to yell (e.g. old carbide-admin-cli).
        convert_and_log_machine_id(request.host_id.as_ref())?
    };

    if !machine_id.machine_type().is_host() {
        return Err(CarbideError::InvalidArgument(
            "carbide only supports setting the UEFI password on discovered hosts".into(),
        )
        .into());
    }

    let snapshot = db::managed_host::load_snapshot(
        &mut txn,
        &machine_id,
        LoadSnapshotOptions {
            include_history: false,
            include_instance_data: false,
            host_health_config: api.runtime_config.host_health,
        },
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;

    let addr = snapshot.host_snapshot.bmc_addr().ok_or_else(|| {
        CarbideError::InvalidArgument("specified machine does not have BMC address".into())
    })?;

    // A known BMC MAC is a hard precondition for setting the UEFI password: it
    // keys the host_uefi rotation bookkeeping recorded below, so reject the
    // request up front rather than driving the device and only then discovering
    // we cannot track its convergence.
    let host_bmc_mac = snapshot.host_snapshot.status.bmc_info.mac.ok_or_else(|| {
        CarbideError::InvalidArgument(
            "specified machine does not have a known BMC MAC address".into(),
        )
    })?;

    let bmc_access_info =
        db::machine_interface::lookup_bmc_access_info(&mut txn, addr.ip(), Some(addr.port()))
            .await?;

    // Resolve the site-wide host UEFI credential key to set (table-driven; v0 =
    // the legacy unversioned site-default). This is the DB half; do it while the
    // txn is open.
    let host_uefi_key = host_uefi_set_credential_key(&mut txn).await?;

    // Commit before the remote reader (Vault) request and the redfish call so the
    // connection is not held across them, then read the actual secret.
    txn.commit().await?;
    let host_uefi_credentials =
        read_uefi_credentials(api.bmc_credential_ops.credential_reader(), &host_uefi_key).await?;

    let job_id = api
        .bmc_credential_ops
        .uefi_setup(&bmc_access_info, false, host_uefi_credentials)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to run uefi_setup call");
            match e {
                // Keep the Redfish-classified error code (and its operator
                // mitigation text) for client-creation failures.
                CredentialOpError::ClientCreation(inner) => CarbideError::RedfishClientCreation {
                    inner: inner.into(),
                    machine_id,
                },
                e => CarbideError::internal(format!("failed redfish uefi_setup subtask: {e}")),
            }
        })?;
    // uefi_setup returns a BMC job_id; the password change completes
    // asynchronously on the device and we do not poll it here. We optimistically
    // stamp bios_password_set_time and, in the same transaction, record host_uefi
    // convergence (keyed by the host BMC MAC, mirroring the backfill) so the two
    // always agree -- convergence rides along with the pre-existing marker. If
    // the dispatched job ultimately fails on the BMC, both are inaccurate.
    //
    // TODO(credential-rotation): gate both the bios_password_set_time stamp and
    // the host_uefi convergence record on confirmed job_id completion (poll the
    // BMC job rather than trusting dispatch). Whatever confirms completion should
    // perform both updates together -- convergence does not need its own separate
    // write path or operator-facing API; it follows bios_password_set_time.
    api.with_txn(|txn| {
        async move {
            db::machine::update_bios_password_set_time(&machine_id, txn).await?;
            db::credential_rotation::record_device_converged(
                txn,
                host_bmc_mac,
                db::credential_rotation::CredentialRotationType::HostUefi,
            )
            .await?;
            Ok::<(), db::DatabaseError>(())
        }
        .boxed()
    })
    .await?
    .map_err(|e| {
        tracing::error!(
            error = %e,
            "Failed to update bios_password_set_time",
        );
        CarbideError::Internal {
            message: format!("Failed to update BIOS password timestamp: {e}"),
        }
    })?;

    Ok(Response::new(rpc::SetHostUefiPasswordResponse { job_id }))
}

/// Set a DPU's UEFI password directly on the device (the DPU equivalent of
/// [`set_host_uefi_password`]): stage the site-wide `dpu_uefi` credential through
/// the DPU's Redfish BIOS settings and record `dpu_uefi` convergence keyed by the
/// DPU BMC MAC. Like the host path, this stages the change and returns without
/// rebooting -- a DPU UEFI change only takes effect after a DPU restart, which
/// the DPU ingestion flow and the `RotatingDpuUefi` rotation state perform; this
/// direct path exists for operator testing, mirroring `SetHostUefiPassword`.
pub(crate) async fn set_dpu_uefi_password(
    api: &Api,
    request: Request<rpc::SetDpuUefiPasswordRequest>,
) -> Result<Response<rpc::SetDpuUefiPasswordResponse>, Status> {
    log_request_data(&request);

    let mut txn = api.txn_begin().await?;

    let request = request.into_inner();

    let machine_id: MachineId = if let Some(query) = request.machine_query {
        match db::machine::find_by_query(&mut txn, &query).await? {
            Some(machine) => {
                log_machine_id(&machine.id);
                machine.id
            }
            None => {
                return Err(CarbideError::NotFoundError {
                    kind: "machine",
                    id: query,
                }
                .into());
            }
        }
    } else {
        convert_and_log_machine_id(request.dpu_id.as_ref())?
    };

    if !machine_id.machine_type().is_dpu() {
        return Err(CarbideError::InvalidArgument(
            "SetDpuUefiPassword targets a DPU machine; use SetHostUefiPassword for a host".into(),
        )
        .into());
    }

    let snapshot = db::managed_host::load_snapshot(
        &mut txn,
        &machine_id,
        LoadSnapshotOptions {
            include_history: false,
            include_instance_data: false,
            host_health_config: api.runtime_config.host_health,
        },
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;

    // The loaded snapshot is keyed by the host machine; the DPU we are targeting
    // is one of its `dpu_snapshots`. Locate it so we drive the DPU's own BMC.
    let dpu = snapshot
        .dpu_snapshots
        .iter()
        .find(|d| d.id.as_machine_id() == &machine_id)
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "dpu",
            id: machine_id.to_string(),
        })?;

    let addr = dpu.bmc_addr().ok_or_else(|| {
        CarbideError::InvalidArgument("specified DPU does not have BMC address".into())
    })?;

    // A known DPU BMC MAC is a hard precondition: it keys the dpu_uefi rotation
    // bookkeeping recorded below, so reject up front rather than driving the
    // device and only then discovering we cannot track its convergence.
    let dpu_bmc_mac = dpu.status.bmc_info.mac.ok_or_else(|| {
        CarbideError::InvalidArgument("specified DPU does not have a known BMC MAC address".into())
    })?;

    let bmc_access_info =
        db::machine_interface::lookup_bmc_access_info(&mut txn, addr.ip(), Some(addr.port()))
            .await?;

    // Resolve the site-wide DPU UEFI credential key to set (table-driven; v0 =
    // the legacy unversioned site-default). This is the DB half; do it while the
    // txn is open.
    let dpu_uefi_key = dpu_uefi_set_credential_key(&mut txn).await?;

    // Commit before the remote reader (Vault) request and the redfish call so the
    // connection is not held across them, then read the actual secret.
    txn.commit().await?;
    let dpu_uefi_credentials =
        read_uefi_credentials(api.bmc_credential_ops.credential_reader(), &dpu_uefi_key).await?;

    // A DPU stages the UEFI change through Redfish BIOS settings and schedules no
    // job (it commits on the next DPU restart), so there is no job id to return.
    api.bmc_credential_ops
        .uefi_setup(&bmc_access_info, true, dpu_uefi_credentials)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to run uefi_setup call for DPU");
            match e {
                // Keep the Redfish-classified error code (and its operator
                // mitigation text) for client-creation failures.
                CredentialOpError::ClientCreation(inner) => CarbideError::RedfishClientCreation {
                    inner: inner.into(),
                    machine_id,
                },
                e => CarbideError::internal(format!("failed redfish uefi_setup subtask: {e}")),
            }
        })?;

    // Mirror the host path's optimistic convergence record: the change is staged
    // through Redfish BIOS settings and commits on the next DPU restart; record
    // dpu_uefi convergence (keyed by the DPU BMC MAC, as ingestion and the
    // backfill do) so the rotation bookkeeping tracks this DPU. If the staged
    // change ultimately fails to apply, this is inaccurate -- the same
    // optimism the host set path carries.
    api.with_txn(|txn| {
        async move {
            db::credential_rotation::record_device_converged(
                txn,
                dpu_bmc_mac,
                db::credential_rotation::CredentialRotationType::DpuUefi,
            )
            .await?;
            Ok::<(), db::DatabaseError>(())
        }
        .boxed()
    })
    .await?
    .map_err(|e| {
        tracing::error!(error = %e, "Failed to record dpu_uefi convergence");
        CarbideError::Internal {
            message: format!("Failed to record DPU UEFI convergence: {e}"),
        }
    })?;

    Ok(Response::new(rpc::SetDpuUefiPasswordResponse {}))
}
