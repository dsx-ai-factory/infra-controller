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

//! `RotateCredential` / `GetCredentialRotationStatus` handlers.
//!
//! `RotateCredential` *stages* a site-wide rotation: it writes the rotate-TO
//! secret at the next version and then publishes the new `target_version` with a
//! compare-and-set. The current site-wide credential is defined by
//! `sitewide_credential_rotation.target_version`; consumers resolve the live
//! version from that table rather than from a fixed unversioned path. A rotation
//! never writes an unversioned alias. The handler publishes a target only after
//! its immutable secret has been written and read back. Device convergence is
//! owned by each credential family's writer or rotation controller.

use ::rpc::forge as rpc;
use carbide_authn::middleware::Principal;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, Credentials, NicLockdownIkm,
};
use carbide_switch_controller::io::SwitchStateControllerIO;
use db::ConditionalWrite::{Applied, NotApplied};
use db::credential_rotation::RotationTargetNotCurrent;
use mac_address::MacAddress;
use model::switch::SwitchSearchFilter;
use state_controller::io::StateControllerIO;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;

type RotationType = db::credential_rotation::CredentialRotationType;

/// Non-secret request context stored with a published rotation target.
#[derive(serde::Serialize)]
struct RotationRequestMeta {
    reason: Option<String>,
    initiator: Vec<String>,
}

/// Narrows a non-negative DB `integer` to the proto `uint32`, surfacing the
/// (impossible, given the column CHECKs) negative/overflow case as an internal
/// error rather than silently wrapping.
fn to_u32(value: i32, what: &str) -> Result<u32, CarbideError> {
    u32::try_from(value)
        .map_err(|e| CarbideError::internal(format!("{what} {value} is not representable: {e}")))
}

/// [`to_u32`] lifted over an optional column.
fn opt_to_u32(value: Option<i32>, what: &str) -> Result<Option<u32>, CarbideError> {
    value.map(|v| to_u32(v, what)).transpose()
}

/// Non-secret identifiers of the authenticated caller, for the rotation audit
/// record. Returns the empty list when no `AuthContext` is attached (e.g. a test
/// harness that bypasses the auth middleware); the rotation still proceeds since
/// authorization is enforced upstream -- this only enriches `request_meta`.
fn initiator_identifiers<T>(request: &Request<T>) -> Vec<String> {
    request
        .extensions()
        .get::<crate::auth::AuthContext>()
        .map(|ctx| {
            ctx.principals
                .iter()
                .map(Principal::as_identifier)
                .collect()
        })
        .unwrap_or_default()
}

/// Maps a specific proto rotation family onto its database representation.
fn to_rotation_type(credential_type: i32) -> Result<RotationType, CarbideError> {
    let parsed = rpc::RotationCredentialType::try_from(credential_type).map_err(|_| {
        CarbideError::NotFoundError {
            kind: "rotation_credential_type",
            id: credential_type.to_string(),
        }
    })?;
    match parsed {
        rpc::RotationCredentialType::RotationBmc => Ok(RotationType::Bmc),
        rpc::RotationCredentialType::RotationHostUefi => Ok(RotationType::HostUefi),
        rpc::RotationCredentialType::RotationDpuUefi => Ok(RotationType::DpuUefi),
        rpc::RotationCredentialType::RotationLockdownIkm => Ok(RotationType::LockdownIkm),
        rpc::RotationCredentialType::RotationNvos => Ok(RotationType::Nvos),
        rpc::RotationCredentialType::RotationDpuBmcService => Ok(RotationType::DpuBmcService),
        // The proto3 zero value. Rejected rather than defaulted so a caller that
        // omits the family never silently rotates BMC (the most sensitive one).
        rpc::RotationCredentialType::Unspecified => Err(CarbideError::InvalidArgument(
            "credential_type must be set to a specific rotation family".to_string(),
        )),
    }
}

/// The immutable, version-addressed key a rotation writes for a family
/// (`.../v{N}`): the rotation stages version N's secret here and consumers read
/// it back by version. Which version is "current" is resolved from
/// `sitewide_credential_rotation.target_version`, not from any fixed path -- the
/// table-driven contract. Consumers resolve the live key via the per-family
/// helpers rather than depending on a concrete secrets backend.
fn versioned_rotation_key(rotation_type: RotationType, version: u32) -> CredentialKey {
    match rotation_type {
        RotationType::Bmc => CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRootVersioned { version },
        },
        RotationType::HostUefi => CredentialKey::HostUefiSiteVersioned { version },
        RotationType::DpuUefi => CredentialKey::DpuUefiSiteVersioned { version },
        RotationType::LockdownIkm => CredentialKey::NicLockdownIkm {
            credential_type: NicLockdownIkm::SiteWide { version },
        },
        RotationType::Nvos => CredentialKey::switch_nvos_site_admin(version),
        RotationType::DpuBmcService => CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideDpuBmcServiceVersioned { version },
        },
    }
}

/// Stores and publishes the next site-wide credential target.
///
/// The versioned credential is created and read back before its database target
/// is published. Existing targets advance through a version CAS; NVOS publishes
/// version zero through a row-absence CAS when initialized for the first time.
pub(crate) async fn rotate_credential(
    api: &Api,
    request: Request<rpc::RotateCredentialRequest>,
) -> Result<Response<rpc::RotateCredentialResult>, Status> {
    // Do not log_request_data: the request may carry an operator-supplied password.
    // Capture the caller identity for the audit record before consuming the
    // request (the extensions, including the AuthContext, live on the envelope).
    let initiator = initiator_identifiers(&request);
    let req = request.into_inner();
    let credential_type = req.credential_type;
    let rotation_type = to_rotation_type(credential_type)?;

    // Check if the backend supports NVOS password rotation.
    if rotation_type == RotationType::Nvos {
        let Some(component_manager) = api.component_manager.as_ref() else {
            return Err(CarbideError::FailedPrecondition(
                "NVOS rotation requires component manager to be configured".to_string(),
            )
            .into());
        };

        if !component_manager.nv_switch.supports_password_rotation() {
            return Err(CarbideError::FailedPrecondition(
                "NVOS rotation requires a component-manager switch backend that supports password rotation"
                    .to_string(),
            )
            .into());
        }

        require_nvos_credential_sources(api).await?;
    }

    // Resolve the rotate-TO password: an operator-supplied password is validated
    // against the same policy the generator guarantees; otherwise we
    // auto-generate one.
    let operator_supplied_password = req.password.is_some();
    let password = match req.password {
        Some(password) => {
            Credentials::validate_password_strength(&password)
                .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?;
            password
        }
        None => Credentials::generate_password(),
    };
    let new_credentials = Credentials::UsernamePassword {
        // Site-wide credentials carry no username (matches the set-from-factory
        // handlers in `credential.rs`).
        username: String::new(),
        password,
    };

    // Existing targets advance by one. NVOS is initialized at version zero only
    // after that immutable secret has been stored and read back below.
    let mut txn = api.txn_begin().await?;
    let current = db::credential_rotation::current_target_version(&mut txn, rotation_type).await?;

    txn.commit().await?;

    let next_version_i32 = match current {
        Some(current) => current.checked_add(1).ok_or_else(|| {
            CarbideError::internal(format!(
                "rotation target version {current} cannot be advanced"
            ))
        })?,
        None if rotation_type == RotationType::Nvos => 0,
        None => {
            return Err(CarbideError::FailedPrecondition(format!(
                "no site-wide rotation target exists for {rotation_type:?}"
            ))
            .into());
        }
    };

    let next_version = u32::try_from(next_version_i32).map_err(|e| {
        CarbideError::internal(format!(
            "rotation target version {next_version_i32} is not representable: {e}"
        ))
    })?;

    let versioned_credential = versioned_rotation_key(rotation_type, next_version);

    // Crash-safe ordering: write the immutable versioned rotate-TO secret
    // (`.../v{N}`) first, then publish the new target via CAS. Publishing the
    // target last means a device ingested mid-rotation is never recorded as
    // converged to a version whose versioned secret is not already in place. The
    // model is fully table-driven: consumers resolve the live version from
    // `target_version`, so the CAS bump alone makes the new version current --
    // there is no unversioned alias to republish.
    stage_versioned_secret(
        api,
        &versioned_credential,
        &new_credentials,
        operator_supplied_password,
        next_version,
    )
    .await?;

    let request_meta = serde_json::to_value(RotationRequestMeta {
        reason: req.reason,
        initiator,
    })
    .map_err(|e| {
        CarbideError::internal(format!(
            "failed to serialize rotation request metadata: {e}"
        ))
    })?;

    // Publish last. Existing rows use a version CAS; initial NVOS publication
    // uses row absence as its CAS.
    let mut txn = api.txn_begin().await?;

    let staged = match current {
        Some(current) => {
            db::credential_rotation::set_next_target_version(
                &mut txn,
                rotation_type,
                current,
                request_meta,
            )
            .await?
        }
        None => {
            db::credential_rotation::set_initial_target_version(
                &mut txn,
                rotation_type,
                request_meta,
            )
            .await?
        }
    };

    let staged = match staged {
        Applied(staged) => staged,
        NotApplied(RotationTargetNotCurrent) => {
            return Err(CarbideError::ConcurrentModificationError(
                "credential rotation",
                format!("the site-wide target for {rotation_type:?} changed during this rotation"),
            )
            .into());
        }
    };

    if rotation_type == RotationType::Nvos {
        enqueue_nvos_rotation_switches(&mut txn).await?;
    }

    txn.commit().await?;

    Ok(Response::new(rpc::RotateCredentialResult {
        credential_type,
        target_version: u32::try_from(staged.target_version).map_err(|e| {
            CarbideError::internal(format!(
                "staged target version {} is not representable: {e}",
                staged.target_version
            ))
        })?,
        started_at: Some(staged.started_at.into()),
    }))
}

/// Rejects target publication when a live switch currently has no credential source.
///
/// Reconciliation remains authoritative if a credential source changes after
/// this point-in-time check.
async fn require_nvos_credential_sources(api: &Api) -> Result<(), CarbideError> {
    let mut txn = api.txn_begin().await?;
    let gaps = db::credential_rotation::nvos_credential_source_gaps(&mut txn).await?;

    txn.commit().await?;

    let mut missing = Vec::new();

    for (switch_id, bmc_mac_address, malformed_expected_credentials) in gaps {
        let Some(bmc_mac_address) = bmc_mac_address else {
            missing.push(format!("{switch_id} (BMC MAC unavailable)"));
            continue;
        };

        if malformed_expected_credentials {
            missing.push(format!(
                "{bmc_mac_address} (expected-switch NVOS credentials are partial or empty)"
            ));
            continue;
        }

        let credentials = api
            .credential_manager
            .get_credentials(&CredentialKey::SwitchNvosAdmin { bmc_mac_address })
            .await
            .map_err(|error| {
                CarbideError::internal(format!(
                    "failed to check current NVOS credential for {bmc_mac_address}: {error}"
                ))
            })?;

        if !matches!(
            credentials,
            Some(Credentials::UsernamePassword { username, password })
                if !username.is_empty() && !password.is_empty()
        ) {
            missing.push(bmc_mac_address.to_string());
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(CarbideError::FailedPrecondition(format!(
            "NVOS rotation requires current credentials for all live switches; missing \
             credential source for {}",
            missing.join(", ")
        )))
    }
}

/// Wakes live switches after publishing a new NVOS target.
///
/// Enqueuing in the publication transaction gives Ready switches a prompt pass;
/// the controller's periodic scan remains the fallback for an already queued
/// switch whose concurrent pass did not observe this commit.
async fn enqueue_nvos_rotation_switches(
    txn: &mut sqlx::PgConnection,
) -> Result<usize, db::DatabaseError> {
    let switch_ids = db::switch::find_ids(
        &mut *txn,
        SwitchSearchFilter {
            deleted: model::DeletedFilter::Exclude,
            ..Default::default()
        },
    )
    .await?;

    let queued_objects: Vec<String> = switch_ids
        .into_iter()
        .map(|switch_id| switch_id.to_string())
        .collect();

    state_controller::controller::db::queue_objects(
        txn,
        SwitchStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
        &queued_objects,
    )
    .await
}

/// Writes the rotate-TO secret at its versioned path and verifies writer readback.
///
/// `create_credentials` is create-only (write-once per version). A failure means
/// either the slot is already populated -- a concurrent rotation, or a prior
/// attempt that crashed after writing the secret but before publishing the
/// target -- or a genuine store error. We read the slot back to tell them apart
/// without relying on a typed error:
///
/// * slot populated, operator-supplied password matches (or auto-generated):
///   adopt the stored value so a retry idempotently completes the in-flight
///   rotation;
/// * slot populated, operator-supplied password differs: a different rotation
///   already claimed this version, so report a conflict;
/// * slot empty: the create failed for real.
async fn stage_versioned_secret(
    api: &Api,
    versioned_key: &CredentialKey,
    new_credentials: &Credentials,
    operator_supplied_password: bool,
    next_version: u32,
) -> Result<(), CarbideError> {
    let staged = match api
        .credential_manager
        .create_credentials(versioned_key, new_credentials)
        .await
    {
        Ok(()) => new_credentials.clone(),
        Err(create_err) => match api
            .credential_manager
            .get_credentials_from_writer(versioned_key)
            .await
        {
            Ok(Some(existing)) => {
                if operator_supplied_password && existing != *new_credentials {
                    return Err(CarbideError::ConcurrentModificationError(
                        "credential rotation",
                        format!(
                            "rotate-to version {next_version} is already staged with a \
                             different password"
                        ),
                    ));
                }

                existing
            }
            _ => {
                return Err(CarbideError::internal(format!(
                    "failed to stage the rotate-to secret at version {next_version}: {create_err:?}"
                )));
            }
        },
    };

    let persisted = api
        .credential_manager
        .get_credentials_from_writer(versioned_key)
        .await
        .map_err(|e| {
            CarbideError::internal(format!(
                "failed to read back the rotate-to secret at version {next_version}: {e:?}"
            ))
        })?;

    if persisted.as_ref() != Some(&staged) {
        return Err(CarbideError::internal(format!(
            "rotate-to secret readback did not match version {next_version}"
        )));
    }

    let effective = api
        .credential_manager
        .get_credentials(versioned_key)
        .await
        .map_err(|e| {
            CarbideError::internal(format!(
                "failed to read the effective rotate-to secret at version {next_version}: {e:?}"
            ))
        })?;

    if effective.as_ref() != Some(&staged) {
        return Err(CarbideError::internal(format!(
            "effective rotate-to secret readback did not match version {next_version}"
        )));
    }

    Ok(())
}

/// Reports site-wide or per-device progress toward a credential target.
///
/// NVOS status is evaluated over live switches, including switches whose
/// per-device convergence row has not been established yet.
pub(crate) async fn get_credential_rotation_status(
    api: &Api,
    request: Request<rpc::CredentialRotationStatusRequest>,
) -> Result<Response<rpc::CredentialRotationStatusResult>, Status> {
    crate::api::log_request_data(&request);
    let req = request.into_inner();
    let rotation_type = to_rotation_type(req.credential_type)?;

    // A device_mac scopes the report to a single device; otherwise report the
    // site-wide aggregate.
    if let Some(device_mac) = req.device_mac.as_deref() {
        return device_rotation_status_response(api, rotation_type, device_mac).await;
    }

    let mut txn = api.txn_begin().await?;

    let status = db::credential_rotation::rotation_status(&mut txn, rotation_type)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;

    Ok(Response::new(rpc::CredentialRotationStatusResult {
        target_version: to_u32(status.target_version, "target version")?,
        converged: status.converged.max(0) as u64,
        pending: status.pending.max(0) as u64,
        quarantined: status.quarantined.max(0) as u64,
        quarantined_device_macs: status.quarantined_device_macs,
        started_at: Some(status.started_at.into()),
        // Complete only when every device has reached the target. Quarantined
        // devices are behind the target (in backoff), so they must keep the
        // rotation from reading as complete -- otherwise an operator sees
        // "complete" while devices are still stuck on the old credential.
        complete: status.pending == 0 && status.quarantined == 0,
        device: None,
    }))
}

/// Reports convergence for a single device (matched by `device_mac`) instead of
/// the site-wide aggregate. The count fields describe just this one device (each
/// 0 or 1), and `device` carries the per-device detail. A MAC with no rotation
/// record is a `NotFound` rather than a fabricated "not established" status, so a
/// mistyped MAC is reported instead of silently looking pending.
async fn device_rotation_status_response(
    api: &Api,
    rotation_type: RotationType,
    device_mac: &str,
) -> Result<Response<rpc::CredentialRotationStatusResult>, Status> {
    // Map a malformed MAC to InvalidArgument (a client error); the blanket
    // `CarbideError::from` for a parse error would otherwise surface as Internal.
    let mac: MacAddress = device_mac.parse::<MacAddress>().map_err(|e| {
        CarbideError::InvalidArgument(format!(
            "device_mac '{device_mac}' is not a valid MAC address: {e}"
        ))
    })?;

    let mut txn = api.txn_begin().await?;

    let status = db::credential_rotation::device_rotation_status(&mut txn, rotation_type, mac)
        .await
        .map_err(CarbideError::from)?;

    txn.commit().await?;

    let status = status.ok_or(CarbideError::NotFoundError {
        kind: "device_credential_rotation",
        id: device_mac.to_string(),
    })?;

    // The queried set is exactly one device, so the aggregate counts collapse to
    // 0/1 and stay consistent with the site-wide definitions (a device is pending
    // only when it is neither converged nor quarantined).
    let pending = !status.converged && !status.quarantined;
    let quarantined_device_macs = if status.quarantined {
        vec![status.device_mac.clone()]
    } else {
        Vec::new()
    };

    let device = rpc::DeviceCredentialRotationStatus {
        device_mac: status.device_mac,
        current_version: opt_to_u32(status.current_version, "current version")?,
        rotating_to_version: opt_to_u32(status.rotating_to_version, "rotating-to version")?,
        converged: status.converged,
        quarantined: status.quarantined,
        quarantined_until: status.quarantined_until.map(Into::into),
        rotate_attempts: to_u32(status.rotate_attempts, "rotate attempts")?,
        last_attempt_at: status.rotate_last_attempt_at.map(Into::into),
        last_error: status.rotate_last_error_redacted,
    };

    Ok(Response::new(rpc::CredentialRotationStatusResult {
        target_version: to_u32(status.target_version, "target version")?,
        converged: u64::from(status.converged),
        pending: u64::from(pending),
        quarantined: u64::from(status.quarantined),
        quarantined_device_macs,
        started_at: Some(status.started_at.into()),
        complete: status.converged && !status.quarantined,
        device: Some(device),
    }))
}
