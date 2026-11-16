/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Core-owned, recoverable virtual-media boot transport.
//!
//! An API request configures a policy; only a lifecycle boundary queues a boot.
//! Every irreversible dispatch follows a committed compare-and-swap. A lost
//! reset response is resolved by current-attempt client evidence, never another
//! reset. Native PXE hosts have no policy row and bypass this module.

use std::time::Duration;

use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool};
use carbide_redfish::virtual_media::{VirtualMediaClient, dispatch_reset_once, verify_artifact};
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
use carbide_utils::redfish::BmcAccessInfo;
use chrono::Utc;
use db::work_lock_manager::{AcquireLockError, WorkLock, WorkLockManagerHandle};
use eyre::{Context, bail};
use mac_address::MacAddress;
use model::expected_machine::ExpectedInterfaceIpAllocation;
use model::host_boot::{HostBootIntent, HostBootPhase, HostBootPurpose};
use model::machine::{
    CleanupState, InstanceState, MachineLastRebootRequested, MachineLastRebootRequestedMode,
    MachineState, ManagedHostState, ManagedHostStateSnapshot,
};
use sqlx::PgPool;
use state_controller::state_handler::StateHandlerError;

const SETTLE_TIMEOUT_SECONDS: i64 = 120;

/// Whether an existing lifecycle may advance past its requested host reboot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleBootProgress {
    /// There is no opt-in policy; preserve the normal PXE power path.
    NativePxe,
    /// Core has queued the attempt, but has not finished dispatching it.
    Waiting,
    /// Core already dispatched this exact lifecycle boot; do not reset again.
    Dispatched,
}

/// Find any external boot policy, including a cancelled policy that blocks boot.
pub async fn has_policy(pool: &PgPool, bmc_mac: MacAddress) -> eyre::Result<bool> {
    Ok(db::host_boot::find_by_bmc_mac(pool, bmc_mac)
        .await?
        .is_some())
}

/// Queue one named lifecycle boot and deduplicate subsequent controller polls.
///
/// A different lifecycle waits for current-attempt evidence and owned-media
/// cleanup before reusing the artifact. Cancellation never falls back to PXE.
pub async fn request_lifecycle_boot(
    pool: &PgPool,
    bmc_mac: MacAddress,
    key: String,
    purpose: HostBootPurpose,
) -> eyre::Result<LifecycleBootProgress> {
    let Some(mut intent) = db::host_boot::find_by_bmc_mac(pool, bmc_mac).await? else {
        return Ok(LifecycleBootProgress::NativePxe);
    };
    if intent.cancelled || intent.phase == HostBootPhase::Cancelled {
        bail!("external boot policy is cancelled; a new generation is required");
    }
    if intent.phase == HostBootPhase::Failed {
        bail!(
            "external boot requires attention: {}",
            intent
                .last_error
                .as_deref()
                .unwrap_or("unknown boot failure")
        );
    }
    if intent.lifecycle_key.as_deref() == Some(key.as_str()) {
        return Ok(
            if matches!(
                intent.phase,
                HostBootPhase::AwaitingAcknowledgement
                    | HostBootPhase::Detaching
                    | HostBootPhase::Completed
            ) {
                LifecycleBootProgress::Dispatched
            } else {
                LifecycleBootProgress::Waiting
            },
        );
    }
    let initial_policy = intent.phase == HostBootPhase::Pending && intent.lifecycle_key.is_none();
    if !initial_policy && !(intent.phase == HostBootPhase::Completed && intent.media_uri.is_none())
    {
        return Ok(LifecycleBootProgress::Waiting);
    }
    if intent.purpose == HostBootPurpose::LocalDisk && purpose != HostBootPurpose::Cleanup {
        // A completed local-disk policy must remain local until another explicit
        // task installs a fresh policy generation.
        bail!("external boot policy has handed control to the local disk");
    }
    intent.begin_lifecycle_attempt(key, purpose);
    let mut connection = pool.acquire().await?;
    if !db::host_boot::update(&mut connection, &intent).await? {
        bail!("boot policy changed while queueing a lifecycle attempt");
    }
    Ok(LifecycleBootProgress::Waiting)
}

/// Select only lifecycle states that explicitly expect a Nico network boot.
/// BIOS, firmware, password and generic power-management restarts are excluded.
pub(crate) fn network_boot_purpose(snapshot: &ManagedHostStateSnapshot) -> Option<HostBootPurpose> {
    match &snapshot.managed_state {
        ManagedHostState::HostInit {
            machine_state:
                MachineState::Measuring { .. }
                | MachineState::WaitingForDiscovery
                | MachineState::Discovered { .. },
        } => Some(HostBootPurpose::Discovery),
        ManagedHostState::WaitingForCleanup {
            cleanup_state: CleanupState::HostCleanup { .. },
            ..
        } => Some(HostBootPurpose::Cleanup),
        ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForRebootToReady,
        } => Some(HostBootPurpose::Installation),
        ManagedHostState::Assigned {
            instance_state:
                InstanceState::WaitingForDpusToUp | InstanceState::BootingWithDiscoveryImage { .. },
        } => Some(HostBootPurpose::Cleanup),
        _ => None,
    }
}

/// Block incidental power actions after cancellation or during an owned boot.
pub(crate) async fn guard_incidental_power(
    pool: &PgPool,
    snapshot: &ManagedHostStateSnapshot,
) -> Result<(), StateHandlerError> {
    let Some(mac) = snapshot.host_snapshot.status.bmc_info.mac else {
        return Ok(());
    };
    let Some(intent) = db::host_boot::find_by_bmc_mac(pool, mac).await? else {
        return Ok(());
    };
    if intent.cancelled
        || matches!(
            intent.phase,
            HostBootPhase::Cancelled | HostBootPhase::Failed
        )
    {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "external boot is stopped; explicitly replace or release its policy before further host power actions"
        )));
    }
    if intent.lifecycle_key.is_some() && !intent.phase.is_terminal() {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "waiting for the current Core-owned boot and media cleanup before incidental host power actions"
        )));
    }
    Ok(())
}

/// Apply a final network-boot boundary from the owning managed-host lifecycle.
///
/// `false` means native PXE should perform its usual reset. `true` means the
/// external transport already dispatched the exact state/version request.
pub(crate) async fn managed_boot_dispatched(
    pool: &PgPool,
    snapshot: &ManagedHostStateSnapshot,
    reason: &str,
    purpose: HostBootPurpose,
) -> Result<bool, StateHandlerError> {
    let Some(mac) = snapshot.host_snapshot.status.bmc_info.mac else {
        return Ok(false);
    };
    let key = format!(
        "{}:{}:{reason}",
        snapshot.host_snapshot.id, snapshot.host_snapshot.state.version
    );
    match request_lifecycle_boot(pool, mac, key, purpose)
        .await
        .map_err(StateHandlerError::GenericError)?
    {
        LifecycleBootProgress::NativePxe => Ok(false),
        LifecycleBootProgress::Dispatched => Ok(true),
        LifecycleBootProgress::Waiting => Err(StateHandlerError::GenericError(eyre::eyre!(
            "waiting for Core-owned virtual-media boot dispatch"
        ))),
    }
}

/// Reconcile active boot requests across Core replicas.
///
/// Expiring work admission is combined with fenced short transactions and
/// durable, non-repeating dispatch boundaries. No pooled connection or database
/// transaction spans BMC or HTTP I/O.
pub async fn reconcile_pending(
    pool: &PgPool,
    redfish_pool: &dyn RedfishClientPool,
    work_locks: &WorkLockManagerHandle,
) -> eyre::Result<()> {
    let intents = db::host_boot::list_active(pool).await?;
    for candidate in intents {
        if candidate.lifecycle_key.is_none()
            && !candidate.cancelled
            && candidate.purpose != HostBootPurpose::LocalDisk
        {
            continue;
        }
        let work_key = format!("host-virtual-media:{}", candidate.expected_machine_id);
        let work_lock = match work_locks.try_acquire_lock(work_key).await {
            Ok(lock) => lock,
            Err(AcquireLockError::WorkAlreadyLocked(_)) => continue,
            Err(error) => return Err(error.into()),
        };
        if let Err(error) = reconcile_one(
            pool,
            redfish_pool,
            &work_lock,
            candidate.expected_machine_id,
        )
        .await
        {
            let diagnostic = sanitized_error(&error);
            tracing::warn!(expected_machine_id = %candidate.expected_machine_id, error = %diagnostic, "Virtual-media boot reconciliation deferred");
            if let Some(mut latest) =
                db::host_boot::find(pool, candidate.expected_machine_id).await?
            {
                if latest.generation == candidate.generation
                    && latest.acknowledgement_token == candidate.acknowledgement_token
                    && !latest.cancelled
                    && !latest.acknowledged
                    && !latest.phase.is_terminal()
                    && latest.phase != HostBootPhase::Failed
                {
                    latest.last_error = Some(diagnostic);
                    let mut txn = pool.begin().await?;
                    work_lock.fence_transaction(&mut txn).await?;
                    db::host_boot::update(&mut txn, &latest).await?;
                    txn.commit().await?;
                }
            }
        }
    }
    Ok(())
}

fn sanitized_error(error: &eyre::Report) -> String {
    match error.downcast_ref::<libredfish::RedfishError>() {
        Some(libredfish::RedfishError::NotSupported(message)) => message.clone(),
        Some(libredfish::RedfishError::HTTPErrorCode { status_code, .. }) => {
            format!("BMC or artifact request returned HTTP {status_code}")
        }
        Some(
            libredfish::RedfishError::NetworkError { .. }
            | libredfish::RedfishError::ReqwestError(_),
        ) => "BMC or bootstrap-artifact network request failed; check connectivity and TLS"
            .to_owned(),
        Some(_) => "BMC response could not be read; check capabilities and credentials".to_owned(),
        None => "boot reconciliation could not access its authenticated BMC or persisted state"
            .to_owned(),
    }
}

async fn save(
    pool: &PgPool,
    work_lock: &WorkLock,
    intent: &mut HostBootIntent,
) -> eyre::Result<()> {
    let mut txn = pool.begin().await?;
    work_lock.fence_transaction(&mut txn).await?;
    if !db::host_boot::update(&mut txn, intent).await? {
        txn.rollback().await?;
        bail!("boot intent changed concurrently; reload before any external write");
    }
    txn.commit().await?;
    intent.revision += 1;
    Ok(())
}

async fn fail(
    pool: &PgPool,
    work_lock: &WorkLock,
    intent: &mut HostBootIntent,
    message: &str,
) -> eyre::Result<()> {
    intent.failed_from_phase = Some(intent.phase);
    intent.phase = HostBootPhase::Failed;
    intent.last_error = Some(message.to_owned());
    save(pool, work_lock, intent).await
}

fn settling(intent: &HostBootIntent) -> bool {
    intent
        .phase_started_at
        .is_some_and(|started| (Utc::now() - started).num_seconds() < SETTLE_TIMEOUT_SECONDS)
}

async fn reconcile_one(
    pool: &PgPool,
    redfish_pool: &dyn RedfishClientPool,
    work_lock: &WorkLock,
    id: uuid::Uuid,
) -> eyre::Result<()> {
    let Some(mut intent) = db::host_boot::find(pool, id).await? else {
        return Ok(());
    };
    if matches!(
        intent.phase,
        HostBootPhase::Completed | HostBootPhase::Cancelled
    ) {
        return Ok(());
    }
    if intent.phase == HostBootPhase::Failed {
        if !intent.cancelled {
            return Ok(());
        }
        // Explicit cancellation may resume observation and owned-state cleanup,
        // never the failed forward action. Preserve uncertain write boundaries.
        intent.phase = if intent.reset_attempted_at.is_some() && !intent.acknowledged {
            HostBootPhase::AwaitingAcknowledgement
        } else {
            match intent.failed_from_phase {
                Some(HostBootPhase::Mounting) => HostBootPhase::Mounting,
                Some(HostBootPhase::SettingBoot) => HostBootPhase::SettingBoot,
                _ => HostBootPhase::Detaching,
            }
        };
        save(pool, work_lock, &mut intent).await?;
        return Ok(());
    }
    if intent.cancelled
        && intent.phase == HostBootPhase::Detaching
        && intent.media_uri.is_none()
        && intent.previous_boot.is_none()
        && (intent.reset_attempted_at.is_none() || intent.acknowledged)
    {
        // No owned external state remains. In particular, cancelling a policy
        // before any dispatch must not require VirtualMedia-capable hardware.
        intent.phase = HostBootPhase::Cancelled;
        intent.phase_started_at = None;
        intent.failed_from_phase = None;
        intent.last_error = None;
        save(pool, work_lock, &mut intent).await?;
        return Ok(());
    }
    let expected = db::expected_machine::find_by_id(pool, id)
        .await?
        .ok_or_else(|| eyre::eyre!("expected machine no longer exists"))?;
    if expected.bmc_mac_address != intent.bmc_mac_address {
        return fail(
            pool,
            work_lock,
            &mut intent,
            "expected-machine BMC identity changed",
        )
        .await;
    }
    if !expected.data.interfaces.iter().any(|interface| {
        interface.role.is_host()
            && interface.mac_address == Some(intent.boot_mac_address)
            && interface.resolved_ip_allocation() == ExpectedInterfaceIpAllocation::Fixed
            && interface.fixed_ip == Some(intent.fixed_ip_address)
    }) {
        return fail(
            pool,
            work_lock,
            &mut intent,
            "registered fixed host MAC/IP changed during the boot request",
        )
        .await;
    }
    let ip = db::host_boot::resolve_bmc_address(pool, &expected)
        .await?
        .ok_or_else(|| eyre::eyre!("registered BMC address is missing or ambiguous"))?;
    let access = BmcAccessInfo {
        host: ip.to_string(),
        port: Some(443),
        mac_address: intent.bmc_mac_address,
    };
    let credentials = redfish_pool
        .credential_reader()
        .get_credentials(&CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot {
                bmc_mac_address: intent.bmc_mac_address,
            },
        })
        .await?
        .unwrap_or_else(|| Credentials::UsernamePassword {
            username: expected.data.bmc_username.clone(),
            password: expected.data.bmc_password.clone(),
        });
    let Credentials::UsernamePassword { username, password } = credentials.clone();
    let client = redfish_pool
        .create_client(
            &access.host,
            access.port,
            RedfishAuth::Direct(username, password),
            None,
        )
        .await
        .wrap_err("create authenticated virtual-media BMC client")?;
    let media = VirtualMediaClient::new(client.as_ref());
    let observation = match media.observe().await {
        Ok(observation) => observation,
        Err(libredfish::RedfishError::NotSupported(message)) => {
            return fail(pool, work_lock, &mut intent, &message).await;
        }
        Err(error) => return Err(error.into()),
    };
    if !expected.data.serial_number.trim().is_empty()
        && observation.serial_number.as_deref().map(str::trim)
            != Some(expected.data.serial_number.trim())
    {
        return fail(
            pool,
            work_lock,
            &mut intent,
            "BMC ComputerSystem serial does not match the registered machine",
        )
        .await;
    }
    if observation
        .architecture
        .as_deref()
        .is_some_and(|architecture| {
            !matches!(
                architecture.to_ascii_lowercase().as_str(),
                "x86" | "x86_64" | "amd64"
            )
        })
    {
        return fail(pool, work_lock, &mut intent, "BMC reports an unsupported processor architecture; only x86_64 boot artifacts are supported").await;
    }

    if intent.cancelled && intent.phase != HostBootPhase::Detaching {
        let unresolved_dispatch = match intent.phase {
            HostBootPhase::Mounting => !observation.slots.iter().any(|slot| {
                Some(&slot.uri) == intent.media_uri.as_ref() && slot.contains(&intent.artifact_url)
            }),
            HostBootPhase::SettingBoot => {
                let target = if intent.purpose == HostBootPurpose::LocalDisk {
                    "Hdd"
                } else {
                    "Cd"
                };
                !observation.one_shot_boot_armed(target)
            }
            HostBootPhase::Resetting | HostBootPhase::AwaitingAcknowledgement => {
                !intent.acknowledged
            }
            _ => false,
        };
        if unresolved_dispatch {
            if !matches!(
                intent.phase,
                HostBootPhase::Resetting | HostBootPhase::AwaitingAcknowledgement
            ) && !settling(&intent)
            {
                let message = "cancellation is waiting for an uncertain external write; inspect the owned BMC state before resuming";
                if intent.last_error.as_deref() != Some(message) {
                    intent.last_error = Some(message.to_owned());
                    save(pool, work_lock, &mut intent).await?;
                }
                return Ok(());
            }
            if intent.last_error.is_none() {
                intent.last_error = Some("cancellation requested; waiting for proof that an earlier external dispatch finished before cleaning owned media".to_owned());
                save(pool, work_lock, &mut intent).await?;
            }
            return Ok(());
        }
        intent.phase = HostBootPhase::Detaching;
        intent.phase_started_at = None;
        save(pool, work_lock, &mut intent).await?;
        return Ok(());
    }

    match intent.phase {
        HostBootPhase::Pending => {
            if intent.lifecycle_key.is_none() && intent.purpose != HostBootPurpose::LocalDisk {
                return Ok(());
            }
            if intent.purpose == HostBootPurpose::LocalDisk {
                intent.phase = HostBootPhase::Detaching;
            } else {
                match verify_artifact(&intent.artifact_url, &intent.artifact_sha256).await {
                    Ok(()) => {}
                    Err(libredfish::RedfishError::NotSupported(message)) => {
                        return fail(pool, work_lock, &mut intent, &message).await;
                    }
                    Err(error) => return Err(error.into()),
                }
                intent.phase = HostBootPhase::Inspecting;
            }
            intent.phase_started_at = None;
            save(pool, work_lock, &mut intent).await?;
        }
        HostBootPhase::Inspecting => {
            if !observation.supports_uefi_once("Cd") || !observation.supports_uefi_once("Hdd") {
                return fail(
                    pool,
                    work_lock,
                    &mut intent,
                    "BMC does not prove support for one-shot UEFI optical and local-disk boot overrides",
                )
                .await;
            }
            // Cd selects an optical device rather than a slot URI; another mounted
            // optical medium makes the selection ambiguous even if a free slot exists.
            if observation
                .slots
                .iter()
                .any(|slot| slot.optical && (slot.inserted || slot.image.is_some()))
            {
                return fail(
                    pool,
                    work_lock,
                    &mut intent,
                    "BMC already has mounted optical media; Core will not take it over",
                )
                .await;
            }
            let Some(slot) = observation.slots.iter().find(|slot| slot.available()) else {
                return fail(
                    pool,
                    work_lock,
                    &mut intent,
                    "BMC has no empty supported optical virtual-media slot",
                )
                .await;
            };
            intent.media_uri = Some(slot.uri.clone());
            intent.previous_boot = Some(observation.boot.clone());
            intent.phase = HostBootPhase::Mounting;
            intent.phase_started_at = Some(Utc::now());
            save(pool, work_lock, &mut intent).await?;
            if media.insert(slot, &intent.artifact_url).await.is_err() {
                intent.last_error = Some(
                    "media insertion result is uncertain; waiting for BMC readback".to_owned(),
                );
                save(pool, work_lock, &mut intent).await?;
            }
        }
        HostBootPhase::Mounting => {
            let slot = observation
                .slots
                .iter()
                .find(|slot| Some(&slot.uri) == intent.media_uri.as_ref());
            if slot.is_some_and(|slot| slot.contains(&intent.artifact_url)) {
                intent.phase = HostBootPhase::Mounted;
                intent.phase_started_at = None;
                intent.last_error = None;
                save(pool, work_lock, &mut intent).await?;
            } else if !settling(&intent) {
                return fail(
                    pool,
                    work_lock,
                    &mut intent,
                    "media insertion was not confirmed; inspect the BMC before retrying",
                )
                .await;
            }
        }
        HostBootPhase::Mounted => {
            if !observation.slots.iter().any(|slot| {
                Some(&slot.uri) == intent.media_uri.as_ref() && slot.contains(&intent.artifact_url)
            }) {
                return fail(
                    pool,
                    work_lock,
                    &mut intent,
                    "owned media changed before boot was armed",
                )
                .await;
            }
            if observation.optical_boot_armed() {
                intent.phase = HostBootPhase::Armed;
                intent.phase_started_at = None;
                save(pool, work_lock, &mut intent).await?;
                return Ok(());
            }
            intent.phase = HostBootPhase::SettingBoot;
            intent.phase_started_at = Some(Utc::now());
            save(pool, work_lock, &mut intent).await?;
            if media.arm(&observation, "Cd").await.is_err() {
                intent.last_error =
                    Some("boot override result is uncertain; waiting for BMC readback".to_owned());
                save(pool, work_lock, &mut intent).await?;
            }
        }
        HostBootPhase::SettingBoot => {
            let target = if intent.purpose == HostBootPurpose::LocalDisk {
                "Hdd"
            } else {
                "Cd"
            };
            let armed = observation.one_shot_boot_armed(target);
            if armed {
                intent.phase = if intent.purpose == HostBootPurpose::LocalDisk {
                    HostBootPhase::Completed
                } else {
                    HostBootPhase::Armed
                };
                if intent.purpose == HostBootPurpose::LocalDisk {
                    intent.media_uri = None;
                    intent.previous_boot = None;
                    intent.failed_from_phase = None;
                }
                intent.phase_started_at = None;
                intent.last_error = None;
                save(pool, work_lock, &mut intent).await?;
            } else if !settling(&intent) {
                return fail(
                    pool,
                    work_lock,
                    &mut intent,
                    "one-shot boot override was not confirmed; no reset was sent",
                )
                .await;
            }
        }
        HostBootPhase::Armed => {
            if !observation.optical_boot_armed()
                || !observation.slots.iter().any(|slot| {
                    Some(&slot.uri) == intent.media_uri.as_ref()
                        && slot.contains(&intent.artifact_url)
                })
            {
                return fail(
                    pool,
                    work_lock,
                    &mut intent,
                    "owned media or boot override changed before reset",
                )
                .await;
            }
            if !matches!(observation.power_state.as_deref(), Some("On" | "Off")) {
                return Ok(());
            }
            intent.phase = HostBootPhase::Resetting;
            let requested_at = db::machine::database_now(pool).await?;
            intent.reset_attempted_at = Some(requested_at);
            save(pool, work_lock, &mut intent).await?;
            {
                let mut txn = pool.begin().await?;
                work_lock.fence_transaction(&mut txn).await?;
                if let Some(machine_id) = db::machine::find_id_by_bmc_ip(&mut txn, &ip).await? {
                    db::machine::record_reboot_request(
                        &machine_id,
                        &mut txn,
                        &MachineLastRebootRequested {
                            time: requested_at,
                            mode: if observation.power_state.as_deref() == Some("Off") {
                                MachineLastRebootRequestedMode::PowerOn
                            } else {
                                MachineLastRebootRequestedMode::Reboot
                            },
                            // Current-token Scout/installer evidence owns verification.
                            restart_verified: None,
                            verification_attempts: Some(0),
                        },
                    )
                    .await?;
                }
                txn.commit().await?;
            }
            // The committed Resetting phase is an at-most-once dispatch fence.
            // A crash before this call trades automatic liveness for avoiding a
            // duplicate reset after an unknown result.
            let result = dispatch_reset_once(
                &access,
                credentials,
                &observation.system_uri,
                observation.power_state.as_deref() == Some("Off"),
            )
            .await;
            intent.phase = HostBootPhase::AwaitingAcknowledgement;
            intent.last_error = result.err().map(|_| {
                "reset result is uncertain; waiting for current-attempt evidence".to_owned()
            });
            save(pool, work_lock, &mut intent).await?;
        }
        HostBootPhase::Resetting => {
            intent.phase = HostBootPhase::AwaitingAcknowledgement;
            intent.last_error = Some("reset dispatch outcome is uncertain; waiting for current-attempt evidence without repeating reset".to_owned());
            save(pool, work_lock, &mut intent).await?;
        }
        HostBootPhase::AwaitingAcknowledgement => {
            if intent.acknowledged {
                intent.phase = HostBootPhase::Detaching;
                intent.phase_started_at = None;
                save(pool, work_lock, &mut intent).await?;
            } else if intent
                .reset_attempted_at
                .is_some_and(|started| (Utc::now() - started).num_minutes() >= 30)
                && intent.last_error.is_none()
            {
                intent.last_error = Some("no current-attempt client evidence after 30 minutes; reset will not be repeated automatically".to_owned());
                save(pool, work_lock, &mut intent).await?;
            }
        }
        HostBootPhase::Detaching => {
            if !intent.cancelled
                && !intent.acknowledged
                && intent.purpose != HostBootPurpose::LocalDisk
            {
                return fail(
                    pool,
                    work_lock,
                    &mut intent,
                    "owned media cannot be detached before current-attempt evidence",
                )
                .await;
            }
            if let Some(uri) = &intent.media_uri {
                let Some(slot) = observation.slots.iter().find(|slot| &slot.uri == uri) else {
                    return fail(
                        pool,
                        work_lock,
                        &mut intent,
                        "owned virtual-media slot disappeared during cleanup",
                    )
                    .await;
                };
                if slot.contains(&intent.artifact_url) {
                    if intent.phase_started_at.is_none() {
                        intent.phase_started_at = Some(Utc::now());
                        save(pool, work_lock, &mut intent).await?;
                        if media.eject(slot, &intent.artifact_url).await.is_err() {
                            intent.last_error = Some(
                                "media ejection result is uncertain; waiting for BMC readback"
                                    .to_owned(),
                            );
                            save(pool, work_lock, &mut intent).await?;
                        }
                    } else if !settling(&intent) {
                        return fail(
                            pool,
                            work_lock,
                            &mut intent,
                            "owned media ejection was not confirmed; no other slot was touched",
                        )
                        .await;
                    }
                    return Ok(());
                }
                if slot.inserted || slot.image.is_some() {
                    return fail(
                        pool,
                        work_lock,
                        &mut intent,
                        "virtual-media slot now contains another image; cleanup stopped",
                    )
                    .await;
                }
                intent.media_uri = None;
                intent.phase_started_at = None;
                save(pool, work_lock, &mut intent).await?;
                return Ok(());
            }
            if intent.purpose == HostBootPurpose::LocalDisk && !intent.cancelled {
                intent.previous_boot = Some(observation.boot.clone());
                intent.phase = HostBootPhase::SettingBoot;
                intent.phase_started_at = Some(Utc::now());
                save(pool, work_lock, &mut intent).await?;
                if media.arm(&observation, "Hdd").await.is_err() {
                    intent.last_error = Some(
                        "local-disk override result is uncertain; waiting for BMC readback"
                            .to_owned(),
                    );
                    save(pool, work_lock, &mut intent).await?;
                }
                return Ok(());
            }
            if let Some(previous) = &intent.previous_boot {
                let fields_match = [
                    "BootSourceOverrideTarget",
                    "BootSourceOverrideEnabled",
                    "BootSourceOverrideMode",
                ]
                .iter()
                .all(|field| {
                    previous
                        .get(field)
                        .is_none_or(|value| observation.boot.get(field) == Some(value))
                });
                let consumed = observation
                    .boot
                    .get("BootSourceOverrideEnabled")
                    .and_then(serde_json::Value::as_str)
                    == Some("Disabled");
                if fields_match || consumed {
                    // A consumed Once override must not be re-enabled merely
                    // to reproduce an earlier transient override.
                } else if observation.one_shot_boot_armed(
                    if intent.purpose == HostBootPurpose::LocalDisk {
                        "Hdd"
                    } else {
                        "Cd"
                    },
                ) {
                    media
                        .restore_boot(
                            &observation,
                            previous,
                            if intent.purpose == HostBootPurpose::LocalDisk {
                                "Hdd"
                            } else {
                                "Cd"
                            },
                        )
                        .await?;
                    return Ok(());
                } else {
                    return fail(pool, work_lock, &mut intent, "boot override changed by another actor; owned-media cleanup stopped before altering it").await;
                }
            }
            intent.previous_boot = None;
            intent.phase = if intent.cancelled {
                HostBootPhase::Cancelled
            } else {
                HostBootPhase::Completed
            };
            intent.phase_started_at = None;
            intent.last_error = None;
            intent.failed_from_phase = None;
            save(pool, work_lock, &mut intent).await?;
        }
        HostBootPhase::Completed | HostBootPhase::Cancelled | HostBootPhase::Failed => {}
    }
    Ok(())
}

/// Default delay between boot-controller sweeps.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);
