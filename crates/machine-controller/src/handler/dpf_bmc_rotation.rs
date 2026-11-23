// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use carbide_credential_rotation::{RotationStep, advance, site_explorer_pause};
use carbide_dpf::{BmcCredentialFailureReason, BmcCredentialHealth, BmcCredentialObservation};
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, Credentials, DpfBmcRotationCandidate,
};
use chrono::Utc;
use db::credential_rotation::CredentialRotationType;
use db::{TransactionVending as _, dpf_bmc_rotation as recovery};
use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression};
use model::machine::dpf_bmc_rotation::{
    DpfBmcRecoveryStep, DpfBmcRotation, DpfBmcRotationPhase, DpfDpuBmcRotation,
    DpfDpuBmcRotationStep,
};
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{ManagedHostState, ManagedHostStateSnapshot};
use sqlx::PgConnection;
use state_controller::state_handler::{StateHandlerError, StateHandlerOutcome};
use uuid::Uuid;

use super::rotation;
use crate::context::MachineStateHandlerServices;
use crate::dpf::DpfOperations;

const SOURCE: BmcSuppressionSource = BmcSuppressionSource::BmcCredentialRotation;
const SHARED_SECRET: &str = "bmc-shared-password";
const REFERENCE_UPDATE_TIMEOUT: Duration = Duration::from_secs(30);
const REFERENCE_RETRY_DELAY: chrono::Duration = chrono::Duration::seconds(5);

type Outcome = StateHandlerOutcome<ManagedHostState>;

pub(super) fn start(mh: &ManagedHostStateSnapshot) -> DpfBmcRotation {
    DpfBmcRotation {
        bmc_macs: mh
            .host_snapshot
            .status
            .bmc_info
            .mac
            .into_iter()
            .chain(
                mh.dpu_snapshots
                    .iter()
                    .filter_map(|dpu| dpu.status.bmc_info.mac),
            )
            .collect(),
        phase: DpfBmcRotationPhase::SuppressExploration,
    }
}

pub(super) async fn handle(
    mh: &ManagedHostStateSnapshot,
    services: &MachineStateHandlerServices,
    sdk: Option<&dyn DpfOperations>,
    mut retry_count: u32,
    rotation: &DpfBmcRotation,
) -> Result<Outcome, StateHandlerError> {
    use DpfBmcRotationPhase as Phase;

    let Some(sdk) =
        sdk.filter(|_| services.site_config.dpf_enabled && services.dpf_per_dpu_bmc_credentials)
    else {
        return Ok(Outcome::wait(
            "waiting for DPF BMC credential support".to_string(),
        ));
    };
    let host = &mh.host_snapshot;
    let host_id = host.id.to_string();
    let mut txn = services
        .db_pool
        .txn_begin()
        .await
        .map_err(StateHandlerError::from_transaction)?;
    recovery::lock_host(&mut txn, &host_id).await?;
    if !db::machine::find_one(&mut *txn, &host.id, MachineSearchConfig::default())
        .await?
        .is_some_and(|live| live.state.version == host.state.version)
    {
        return Ok(Outcome::do_nothing());
    }

    let phase = match &rotation.phase {
        Phase::SuppressExploration => {
            for &mac in &rotation.bmc_macs {
                db::bmc_suppression::upsert(
                    &mut txn,
                    &NewBmcSuppression {
                        bmc_mac_address: mac,
                        subsystem: BmcSuppressionSubsystem::SiteExplorer,
                        source: SOURCE,
                        reason: "DPF BMC credential rotation".to_string(),
                    },
                )
                .await?;
            }
            Phase::AwaitExplorationSuppression
        }
        Phase::AwaitExplorationSuppression => {
            let rows = db::bmc_suppression::find_many(
                &mut *txn,
                &rotation.bmc_macs,
                BmcSuppressionSubsystem::SiteExplorer,
                SOURCE,
            )
            .await?;
            if !rotation.bmc_macs.iter().all(|mac| {
                rows.iter()
                    .any(|row| row.bmc_mac_address == *mac && row.acknowledged_at.is_some())
            }) {
                return Ok(Outcome::wait(
                    "waiting for site-explorer to acknowledge the BMC rotation suppression"
                        .to_string(),
                ));
            }
            Phase::RotateHost
        }
        Phase::RotateHost => {
            match advance(
                rotation::rotate_managed_host_bmcs(services, mh).await,
                retry_count,
                host.id,
            ) {
                RotationStep::WaitForCredentialStoreReconcile => {
                    return Ok(Outcome::wait(
                        "waiting for host BMC credential store reconciliation".to_string(),
                    ));
                }
                RotationStep::Retry { retry_count: next } => {
                    retry_count = next;
                    Phase::RotateHost
                }
                RotationStep::Settled => {
                    if host.bmc_credential_rotation_requested {
                        db::machine::clear_bmc_credential_rotation_requested(
                            &mut txn,
                            &host.id.into(),
                        )
                        .await?;
                    }
                    Phase::RotateDpus {
                        completed: Vec::new(),
                        current: None,
                    }
                }
                RotationStep::GaveUp => Phase::RotateDpus {
                    completed: Vec::new(),
                    current: None,
                },
            }
        }
        Phase::RotateDpus {
            completed,
            current: None,
        } => match select_dpu(&mut txn, mh, services, sdk, completed).await? {
            Selection::Work(current) => Phase::RotateDpus {
                completed: completed.clone(),
                current: Some(Box::new(current)),
            },
            Selection::Wait(reason) => return Ok(Outcome::wait(reason)),
            Selection::Complete => Phase::ResumeExploration,
        },
        Phase::RotateDpus {
            current: Some(current),
            ..
        } if !mh.dpu_snapshots.iter().any(|dpu| {
            dpu.id == current.dpu_machine_id && dpu.status.bmc_info.mac == Some(current.device_mac)
        }) =>
        {
            return Ok(Outcome::wait(
                "waiting for the rotation DPU to match the host topology".to_string(),
            ));
        }
        Phase::RotateDpus {
            completed,
            current: Some(current),
        } => match rotate_dpu(&mut txn, services, sdk, current).await? {
            Progress::Advance(step) => {
                let mut current = current.clone();
                current.step = step;
                Phase::RotateDpus {
                    completed: completed.clone(),
                    current: Some(current),
                }
            }
            Progress::Wait(reason) => return Ok(Outcome::wait(reason)),
            Progress::Complete => {
                let mut completed = completed.clone();
                completed.push(current.dpu_machine_id);
                Phase::RotateDpus {
                    completed,
                    current: None,
                }
            }
        },
        Phase::ResumeExploration => {
            site_explorer_pause::resume_after_credential_change(
                &mut txn,
                &rotation.bmc_macs,
                SOURCE,
            )
            .await?;
            return Ok(Outcome::transition(ManagedHostState::Ready).with_txn(txn));
        }
    };
    Ok(Outcome::transition(ManagedHostState::RotatingBmc {
        retry_count,
        dpf_rotation: Some(Box::new(DpfBmcRotation {
            bmc_macs: rotation.bmc_macs.clone(),
            phase,
        })),
    })
    .with_txn(txn))
}

enum Selection {
    Work(DpfDpuBmcRotation),
    Wait(String),
    Complete,
}

async fn select_dpu(
    txn: &mut PgConnection,
    mh: &ManagedHostStateSnapshot,
    services: &MachineStateHandlerServices,
    sdk: &dyn DpfOperations,
    completed: &[carbide_uuid::machine::DpuMachineId],
) -> Result<Selection, StateHandlerError> {
    let host = &mh.host_snapshot;
    for dpu in &mh.dpu_snapshots {
        let forced = dpu.bmc_credential_rotation_requested;
        if completed.contains(&dpu.id) || !(forced || services.site_config.bmc_rotation_enabled) {
            continue;
        }
        let Some(device_mac) = dpu.status.bmc_info.mac else {
            if forced {
                return Ok(Selection::Wait(format!(
                    "DPU {} is missing its BMC MAC",
                    dpu.id
                )));
            }
            continue;
        };
        let status = db::credential_rotation::device_rotation_status(
            &mut *txn,
            CredentialRotationType::Bmc,
            device_mac,
        )
        .await?;
        if !forced
            && !status
                .as_ref()
                .is_some_and(|status| !status.converged && !status.quarantined)
        {
            continue;
        }
        if status
            .as_ref()
            .is_some_and(|status| status.rotating_to_version.is_some())
        {
            return Ok(Selection::Wait(format!(
                "waiting for prior BMC rotation on DPU {}",
                dpu.id
            )));
        }
        let device_id = device_id(device_mac);
        let observation = sdk
            .observe_bmc_credential(&device_id)
            .await
            .map_err(|_| retry("read DPUDevice credential status"))?;
        let previous_secret = observation
            .desired_secret_name
            .filter(|name| name != SHARED_SECRET);
        if observation.active_secret_name.as_deref()
            != Some(previous_secret.as_deref().unwrap_or(SHARED_SECRET))
            || observation.health != BmcCredentialHealth::Ready
        {
            return Ok(Selection::Wait(format!(
                "waiting for DPF to confirm the current credential for DPU {}",
                dpu.id
            )));
        }

        let target_version = match &status {
            Some(status) => status.target_version,
            None => db::credential_rotation::current_target_version(
                &mut *txn,
                CredentialRotationType::Bmc,
            )
            .await?
            .ok_or_else(|| retry("read BMC rotation target version"))?,
        };
        let version = as_version(target_version)?;
        let target_secret = carbide_dpf::bmc_credential_secret_name(&device_id, Some(version));
        if previous_secret.as_deref() == Some(target_secret.as_str())
            && status
                .as_ref()
                .is_some_and(|status| status.current_version == Some(target_version))
        {
            if forced {
                db::machine::clear_bmc_credential_rotation_requested(txn, &dpu.id.into()).await?;
            }
            continue;
        }
        let current = DpfDpuBmcRotation {
            operation_id: Uuid::new_v4(),
            dpu_machine_id: dpu.id,
            device_mac,
            device_uid: observation.device_uid,
            previous_secret,
            target_secret,
            previous_version: status.and_then(|status| status.current_version),
            target_version,
            forced,
            step: DpfDpuBmcRotationStep::SaveRecoveryCredentials,
        };
        recovery::insert(
            txn,
            &recovery::Recovery {
                operation_id: current.operation_id,
                device_mac,
                host_machine_id: host.id.to_string(),
                host_dpf_id: host.dpf_id().ok_or_else(|| retry("read host BMC MAC"))?,
                previous_version: current.previous_version,
                target_version,
                patch_pending: false,
                detached: false,
            },
        )
        .await?;
        return Ok(Selection::Work(current));
    }
    Ok(Selection::Complete)
}

enum Progress {
    Advance(DpfDpuBmcRotationStep),
    Wait(String),
    Complete,
}

async fn rotate_dpu(
    txn: &mut PgConnection,
    services: &MachineStateHandlerServices,
    sdk: &dyn DpfOperations,
    current: &DpfDpuBmcRotation,
) -> Result<Progress, StateHandlerError> {
    use DpfBmcRecoveryStep as Recover;
    use DpfBmcRotationCandidate::{Previous, Target};
    use DpfDpuBmcRotationStep as Step;

    let store = services.credential_manager.as_ref();
    let device_id = device_id(current.device_mac);
    let key = |candidate| recovery_key(current.operation_id, candidate);
    let root_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: current.device_mac,
        },
    };
    let next = match &current.step {
        Step::SaveRecoveryCredentials => {
            let previous = match store
                .get_credentials_from_writer(&key(Previous))
                .await
                .map_err(|_| retry("read recovery credential"))?
            {
                Some(previous) => previous,
                None => {
                    let credential = read_credential(store, &root_key).await?;
                    store
                        .create_credentials(&key(Previous), &credential)
                        .await
                        .map_err(|_| retry("save previous BMC credential for recovery"))?;
                    credential
                }
            };
            if store
                .get_credentials_from_writer(&key(Target))
                .await
                .map_err(|_| retry("read recovery credential"))?
                .is_none()
            {
                let Credentials::UsernamePassword { username, .. } = previous;
                let Credentials::UsernamePassword { password, .. } = read_credential(
                    store,
                    &CredentialKey::BmcCredentials {
                        credential_type: BmcCredentialType::site_wide_root(as_version(
                            current.target_version,
                        )?),
                    },
                )
                .await?;
                let target = Credentials::new(username, password);
                store
                    .create_credentials(&key(Target), &target)
                    .await
                    .map_err(|_| retry("save target BMC credential for recovery"))?;
            }
            Step::EnsureTargetSecret
        }
        Step::EnsureTargetSecret => {
            ensure_target(sdk, store, current, &device_id).await?;
            recovery::set_patch_pending(txn, current.device_mac, current.operation_id, true)
                .await?;
            Step::UpdateReference
        }
        Step::UpdateReference => {
            let observed = observe(sdk, current, &device_id).await?;
            if observed.desired_secret_name.as_deref() == Some(current.target_secret.as_str()) {
                Step::AwaitAdoption
            } else if observed
                .desired_secret_name
                .as_deref()
                .filter(|name| *name != SHARED_SECRET)
                == current.previous_secret.as_deref()
                && observed.active_secret_name.as_deref()
                    == Some(current.previous_secret.as_deref().unwrap_or(SHARED_SECRET))
                && observed.health == BmcCredentialHealth::Ready
            {
                match tokio::time::timeout(
                    REFERENCE_UPDATE_TIMEOUT,
                    sdk.set_bmc_credential_reference(&observed, &current.target_secret),
                )
                .await
                {
                    Ok(Ok(())) => Step::AwaitAdoption,
                    Ok(Err(_)) | Err(_) => Step::Recover {
                        reason: "waiting to verify uncertain DPUDevice credential update"
                            .to_string(),
                        next: Recover::ObserveReference,
                        retry_at: Some(Utc::now() + REFERENCE_RETRY_DELAY),
                    },
                }
            } else {
                return Ok(Progress::Wait(
                    "waiting for the DPUDevice credential reference to settle before updating it"
                        .to_string(),
                ));
            }
        }
        Step::AwaitAdoption
        | Step::Recover {
            next: Recover::ObserveAdoption,
            ..
        } => {
            let observed = observe(sdk, current, &device_id).await?;
            if observed.is_adopted(&current.target_secret) {
                Step::ReconcileCredential
            } else if observed.desired_secret_name.as_deref()
                != Some(current.target_secret.as_str())
            {
                return Ok(Progress::Wait(
                    "waiting for the intended DPUDevice credential reference".to_string(),
                ));
            } else if let BmcCredentialHealth::Failed(failure) = observed.health {
                let (reason, mut next) = failure_reason(failure);
                if next == Recover::RestoreTargetSecret
                    && sdk
                        .bmc_rotation_secret_exists(&current.target_secret)
                        .await
                        .map_err(|_| retry("check the target DPU BMC credential Secret"))?
                {
                    next = Recover::ObserveAdoption;
                }
                let recovery = Step::Recover {
                    reason: reason.clone(),
                    next,
                    retry_at: None,
                };
                if current.step == recovery {
                    return Ok(Progress::Wait(reason));
                }
                recovery
            } else {
                return Ok(Progress::Wait(
                    "waiting for DPF to adopt the target BMC credential".to_string(),
                ));
            }
        }
        Step::Recover {
            next: Recover::ObserveReference,
            reason,
            retry_at,
        } => {
            let observed = observe(sdk, current, &device_id).await?;
            if observed.desired_secret_name.as_deref() == Some(current.target_secret.as_str()) {
                Step::AwaitAdoption
            } else if retry_at.is_some_and(|time| Utc::now() < time) {
                return Ok(Progress::Wait(reason.clone()));
            } else {
                Step::UpdateReference
            }
        }
        Step::Recover {
            next: Recover::RestoreTargetSecret,
            ..
        } => {
            ensure_target(sdk, store, current, &device_id).await?;
            Step::Recover {
                reason: "waiting for DPF after verifying the target Secret".to_string(),
                next: Recover::ObserveAdoption,
                retry_at: None,
            }
        }
        Step::ReconcileCredential => {
            let target = read_credential(store, &key(Target)).await?;
            store
                .set_credentials(&root_key, &target)
                .await
                .map_err(|_| retry("reconcile the DPU BMC credential store"))?;
            Step::InvalidateSessions
        }
        Step::InvalidateSessions => {
            db::bmc_redfish_session::delete_by_mac(txn, current.device_mac).await?;
            Step::RecordVersion
        }
        Step::RecordVersion => {
            recovery::record_applied_version(txn, current.device_mac, Some(current.target_version))
                .await?;
            recovery::set_patch_pending(txn, current.device_mac, current.operation_id, false)
                .await?;
            Step::DeletePreviousSecret
        }
        Step::DeletePreviousSecret => {
            if let Some(old) = current
                .previous_secret
                .as_deref()
                .filter(|name| *name != current.target_secret)
            {
                observe(sdk, current, &device_id).await?;
                sdk.delete_old_bmc_credential_secret(&device_id, old, &current.target_secret)
                    .await
                    .map_err(|_| retry("delete the inactive DPU BMC credential Secret"))?;
            }
            Step::ForgetRecoveryCredentials
        }
        Step::ForgetRecoveryCredentials => {
            for candidate in [Previous, Target] {
                store
                    .delete_credentials(&key(candidate))
                    .await
                    .map_err(|_| retry("remove the DPU BMC recovery credential"))?;
            }
            recovery::remove(txn, current.device_mac, current.operation_id).await?;
            if current.forced {
                db::machine::clear_bmc_credential_rotation_requested(
                    txn,
                    &current.dpu_machine_id.into(),
                )
                .await?;
            }
            return Ok(Progress::Complete);
        }
    };
    Ok(Progress::Advance(next))
}

async fn ensure_target(
    sdk: &dyn DpfOperations,
    store: &dyn CredentialManager,
    current: &DpfDpuBmcRotation,
    device_id: &str,
) -> Result<(), StateHandlerError> {
    let Credentials::UsernamePassword { password, .. } = read_credential(
        store,
        &recovery_key(current.operation_id, DpfBmcRotationCandidate::Target),
    )
    .await?;
    sdk.ensure_bmc_rotation_secret(device_id, as_version(current.target_version)?, &password)
        .await
        .map_err(|_| retry("create or verify the target DPU BMC credential Secret"))?;
    Ok(())
}

async fn observe(
    sdk: &dyn DpfOperations,
    current: &DpfDpuBmcRotation,
    device_id: &str,
) -> Result<BmcCredentialObservation, StateHandlerError> {
    let observed = sdk
        .observe_bmc_credential(device_id)
        .await
        .map_err(|_| retry("read DPUDevice credential status"))?;
    if observed.device_uid != current.device_uid {
        return Err(retry(
            "DPUDevice identity changed during BMC credential rotation",
        ));
    }
    Ok(observed)
}

async fn read_credential(
    store: &dyn CredentialManager,
    key: &CredentialKey,
) -> Result<Credentials, StateHandlerError> {
    store
        .get_credentials(key)
        .await
        .map_err(|_| retry("read the DPU BMC credential"))?
        .ok_or_else(|| retry("DPU BMC credential is missing"))
}

fn device_id(mac: mac_address::MacAddress) -> String {
    mac.to_string().to_lowercase().replace(':', "-")
}

fn as_version(version: i32) -> Result<u32, StateHandlerError> {
    u32::try_from(version).map_err(|_| retry("invalid BMC credential version"))
}

fn recovery_key(operation_id: Uuid, candidate: DpfBmcRotationCandidate) -> CredentialKey {
    CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::DpfBmcRotation {
            operation_id: *operation_id.as_bytes(),
            candidate,
        },
    }
}

fn retry(message: &str) -> StateHandlerError {
    StateHandlerError::GenericError(eyre::eyre!("{message}"))
}

fn failure_reason(reason: BmcCredentialFailureReason) -> (String, DpfBmcRecoveryStep) {
    use BmcCredentialFailureReason as Reason;
    let reason = match reason {
        Reason::CredentialsSecretNotFound => {
            return (
                "DPF reports CredentialsSecretNotFound".to_string(),
                DpfBmcRecoveryStep::RestoreTargetSecret,
            );
        }
        Reason::CredentialsSecretInvalid => "CredentialsSecretInvalid",
        Reason::BmcAuthenticationFailed => "BMCAuthenticationFailed",
        Reason::ModeSwitchNotAllowed => "ModeSwitchNotAllowed",
        Reason::SharedCredentialSecretNotAllowed => "SharedCredentialSecretNotAllowed",
        Reason::Other(ref reason)
            if reason.len() <= 128
                && reason
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_') =>
        {
            reason
        }
        Reason::Other(_) => "unrecognized credential failure",
    };
    (
        format!("DPF reports {reason}"),
        DpfBmcRecoveryStep::ObserveAdoption,
    )
}
