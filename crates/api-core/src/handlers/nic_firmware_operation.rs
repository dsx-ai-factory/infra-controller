/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Delivery and receipt ingestion for the machine controller's frozen NIC work.
//! This module cannot admit an operation or select a new firmware target.

use carbide_libmlx_model::device::identity::MlxDeviceSelector;
use carbide_libmlx_model::device::info::MlxDeviceInfo;
use carbide_libmlx_model::firmware::FirmwareDeliveryMode;
use carbide_libmlx_model::firmware::operation::FirmwareOperation;
use carbide_uuid::machine::{HostMachineId, MachineId};
use db::ConditionalWrite;
use model::machine::ManagedHostState;
use model::machine::nic_firmware::{
    NicFirmwareActivation, NicFirmwareDeliveryErrorCode, NicFirmwareExecutionOptions,
    NicFirmwareLockdownResult, NicFirmwareLockdownStatus, NicFirmwareLockdownStep,
    NicFirmwareOperation, NicFirmwarePhase, NicFirmwareTarget,
};
use rpc::forge_agent_control_response as fac;
use rpc::protos::mlx_device as pb;
use rpc::protos::mlx_device::mlx_device_update_firmware_result::Result as WireResult;

use super::utils::{StateHandlerWakeupFailed, WakeupTrigger};
use crate::api::Api;
use crate::{CarbideError, CarbideResult};

/// Build only the command authorized by the already committed machine state.
/// Once a receipt is known, polling cannot submit even if the helper disappears.
/// The caller sets `submission_permitted` after checking current ownership and
/// recorded machine failures. When false, updates become observations and a
/// pending unlock change yields no action. Status reads and restoration of the
/// original lockdown policy remain permitted.
/// SiteCache firmware delivery requires the polling Scout's capability;
/// unsupported polls are rejected without changing saved progress. Inventory
/// and restoration do not depend on that transport support.
pub(super) async fn control_action(
    api: &Api,
    host: HostMachineId,
    version: config_version::ConfigVersion,
    operation: &NicFirmwareOperation,
    submission_permitted: bool,
    site_cache_supported: bool,
) -> CarbideResult<fac::Action> {
    operation.validate().map_err(|error| {
        CarbideError::internal(format!("invalid persisted NIC firmware operation: {error}"))
    })?;
    if matches!(operation.phase, NicFirmwarePhase::Reobserving { .. }) {
        return Ok(fac::Action::MlxDeviceInfoReport(
            fac::MlxDeviceInfoReport {},
        ));
    }
    let Some(target) = operation.current_target() else {
        return Ok(fac::Action::noop());
    };
    if operation.delivery_mode == FirmwareDeliveryMode::SiteCache
        && !site_cache_supported
        && matches!(
            operation.phase,
            NicFirmwarePhase::ObservingSupervisor { .. }
                | NicFirmwarePhase::Unlocking { .. }
                | NicFirmwarePhase::UpdatingFirmware { .. }
        )
    {
        return Err(CarbideError::FailedPrecondition(
            "scout does not support the saved NIC firmware SiteCache delivery mode; upgrade scout before continuing".into(),
        ));
    }
    let command = match operation.phase {
        NicFirmwarePhase::Unlocking { .. } | NicFirmwarePhase::RestoringLockdown { .. } => {
            if operation.lockdown.result.is_some()
                || (matches!(operation.phase, NicFirmwarePhase::Unlocking { .. })
                    && operation.lockdown.step == NicFirmwareLockdownStep::Change
                    && !submission_permitted)
            {
                return Ok(fac::Action::noop());
            }
            let desired = operation.desired_lockdown().ok_or_else(|| {
                CarbideError::FailedPrecondition(
                    "NIC firmware lockdown phase has no desired policy".into(),
                )
            })?;
            let request = match crate::nic_firmware::lockdown_request(
                host,
                version,
                target,
                operation.lockdown.step,
                desired,
                &*api.credential_manager,
            )
            .await
            {
                Ok(request) => request,
                Err(
                    error @ (CarbideError::UnavailableError(_)
                    | CarbideError::FailedPrecondition(_)),
                ) => {
                    // Record a bounded cause under this exact revision. A
                    // later poll can retry credentials after reconciliation;
                    // another phase must not inherit this failed rendering.
                    let diagnostic = match error {
                        CarbideError::UnavailableError(_) => {
                            "NIC lockdown credentials are unavailable; retrying the committed request"
                        }
                        _ => "NIC lockdown change requires its frozen key identity",
                    };
                    persist(
                        api,
                        host.into(),
                        &pb::MlxObservationReport {
                            machine_id: Some(host.into()),
                            timestamp: None,
                            observations: vec![pb::MlxObservation {
                                lockdown_result: Some(pb::MlxDeviceLockdownResult {
                                    controller_state_version: version.to_string(),
                                    pci_name: target.pci_name.clone(),
                                    expected_device: Some(target.selector.clone().into()),
                                    result: Some(pb::mlx_device_lockdown_result::Result::Error(
                                        diagnostic.into(),
                                    )),
                                }),
                                ..Default::default()
                            }],
                        },
                    )
                    .await?;
                    return Ok(fac::Action::noop());
                }
                Err(error) => return Err(error),
            };
            fac::mlx_device_action::Command::Lockdown(request)
        }
        NicFirmwarePhase::ObservingSupervisor { .. } => {
            fac::mlx_device_action::Command::ObserveFirmwareUpdate(observe_request(
                operation, target,
            ))
        }
        NicFirmwarePhase::UpdatingFirmware { .. } => {
            if target.receipt.is_some()
                || !submission_permitted
                || target
                    .delivery_error_code
                    .is_some_and(NicFirmwareDeliveryErrorCode::requires_intervention)
            {
                fac::mlx_device_action::Command::ObserveFirmwareUpdate(observe_request(
                    operation, target,
                ))
            } else {
                let instance = target.supervisor_instance_id.as_ref().ok_or_else(|| {
                    CarbideError::FailedPrecondition(
                        "NIC firmware submission has no saved supervisor instance".into(),
                    )
                })?;
                fac::mlx_device_action::Command::UpdateFirmware(fac::MlxDeviceUpdateFirmware {
                    operation_id: operation.operation_id.clone(),
                    phase_id: operation.operation_id.clone(),
                    supervisor_instance_id: Some(instance.clone()),
                    profile: Some(frozen_profile(&operation.options, target)),
                    expected_digests: Some(frozen_digests(target)),
                    expected_device: Some(target.selector.clone().into()),
                    delivery_mode: pb::FirmwareDeliveryMode::from(operation.delivery_mode).into(),
                })
            }
        }
        _ => return Ok(fac::Action::noop()),
    };
    Ok(fac::Action::MlxAction(fac::MlxAction {
        device_actions: vec![fac::MlxDeviceAction {
            pci_name: target.pci_name.clone(),
            command: Some(command),
        }],
    }))
}

/// Observation repeats the same immutable inputs as submission. The
/// supervisor compares its saved request before returning an existing receipt.
fn observe_request(
    operation: &NicFirmwareOperation,
    target: &NicFirmwareTarget,
) -> fac::MlxDeviceObserveFirmwareUpdate {
    fac::MlxDeviceObserveFirmwareUpdate {
        operation_id: operation.operation_id.clone(),
        phase_id: operation.operation_id.clone(),
        profile: Some(frozen_profile(&operation.options, target)),
        expected_digests: Some(frozen_digests(target)),
        expected_device: Some(target.selector.clone().into()),
        delivery_mode: pb::FirmwareDeliveryMode::from(operation.delivery_mode).into(),
    }
}

/// Render the exact admitted recipe, with activation shared by every card.
fn frozen_profile(
    options: &NicFirmwareExecutionOptions,
    target: &NicFirmwareTarget,
) -> pb::FirmwareFlasherProfile {
    let entry = &target.resolution.entry;
    let (reset, reset_level, retry_interval, timeout) = match options.activation {
        NicFirmwareActivation::CardReset {
            reset_level,
            post_reset_verify_retry_interval_seconds,
            post_reset_verify_timeout_seconds,
        } => (
            true,
            reset_level,
            post_reset_verify_retry_interval_seconds,
            post_reset_verify_timeout_seconds,
        ),
        NicFirmwareActivation::HostReboot => (
            false,
            libmlx::firmware::reset::DEFAULT_RESET_LEVEL,
            None,
            None,
        ),
    };
    pb::FirmwareFlasherProfile {
        firmware_spec: Some(entry.firmware.clone().into()),
        flash_spec: Some(pb::FlashSpec {
            firmware_url: entry.image.url.to_string(),
            device_conf_url: entry
                .device_config
                .as_ref()
                .map(|artifact| artifact.url.to_string()),
            ..Default::default()
        }),
        flash_options: Some(pb::FlashOptions {
            verify_image: options.verify_image,
            verify_version: options.verify_version,
            reset,
            reset_level: u32::from(reset_level),
            post_reset_verify_retry_interval_seconds: retry_interval,
            post_reset_verify_timeout_seconds: timeout,
        }),
    }
}

/// Bind both artifacts to admission rather than a later catalog revision.
fn frozen_digests(target: &NicFirmwareTarget) -> pb::FirmwareArtifactDigests {
    let entry = &target.resolution.entry;
    pb::FirmwareArtifactDigests {
        image_sha256: entry.image.sha256.clone(),
        configuration_sha256: entry
            .device_config
            .as_ref()
            .map(|artifact| artifact.sha256.clone()),
    }
}

struct FirmwareUpdate {
    operation_id: String,
    phase_id: String,
    pci_name: String,
    device_info: Option<MlxDeviceInfo>,
    result: FirmwareUpdateResult,
}

enum FirmwareUpdateResult {
    Receipt(FirmwareOperation),
    MissingReceipt(String),
    Error(NicFirmwareDeliveryErrorCode, String),
}

/// Lockdown replies are evidence for one machine-state revision, not reusable
/// lock state. In particular a successful change cannot stand in for readback.
struct FirmwareLockdown {
    version: String,
    pci_name: String,
    expected_device: MlxDeviceSelector,
    device_info: Option<MlxDeviceInfo>,
    result: NicFirmwareLockdownResult,
}

impl TryFrom<&pb::MlxObservation> for FirmwareLockdown {
    type Error = CarbideError;

    fn try_from(observation: &pb::MlxObservation) -> CarbideResult<Self> {
        use pb::mlx_device_lockdown_result::Result as AccessResult;

        if observation.lock_status.is_some()
            || observation.profile_name.is_some()
            || observation.profile_synced.is_some()
            || observation.firmware_report.is_some()
            || observation.firmware_operation.is_some()
            || observation.firmware_update_result.is_some()
        {
            return Err(CarbideError::InvalidArgument(
                "generic NIC lockdown results cannot include firmware or legacy fields".into(),
            ));
        }
        let lockdown = observation.lockdown_result.as_ref().ok_or_else(|| {
            CarbideError::InvalidArgument("NIC firmware lockdown result is required".into())
        })?;
        validate_token(&lockdown.controller_state_version)?;
        let expected_device = lockdown
            .expected_device
            .clone()
            .ok_or_else(|| {
                CarbideError::InvalidArgument("NIC firmware lockdown selector is required".into())
            })?
            .try_into()
            .map_err(
                |error: carbide_libmlx_model::device::identity::MlxDeviceSelectorError| {
                    CarbideError::InvalidArgument(error.to_string())
                },
            )?;
        let device_info = observation
            .device_info
            .clone()
            .map(TryInto::try_into)
            .transpose()
            .map_err(CarbideError::InvalidArgument)?;
        let result = match lockdown.result.as_ref() {
            Some(AccessResult::StatusReport(status)) => {
                if status.device_id != lockdown.pci_name {
                    return Err(CarbideError::InvalidArgument(
                        "NIC firmware lockdown status target contradicts its result".into(),
                    ));
                }
                chrono::DateTime::parse_from_rfc3339(&status.timestamp).map_err(|_| {
                    CarbideError::InvalidArgument(
                        "NIC firmware lockdown status timestamp is invalid".into(),
                    )
                })?;
                match pb::LockStatus::try_from(status.status) {
                    Ok(pb::LockStatus::Locked) => {
                        NicFirmwareLockdownResult::Status(NicFirmwareLockdownStatus::Locked)
                    }
                    Ok(pb::LockStatus::Unlocked) => {
                        NicFirmwareLockdownResult::Status(NicFirmwareLockdownStatus::Unlocked)
                    }
                    Ok(pb::LockStatus::Unknown) => NicFirmwareLockdownResult::Error {
                        diagnostic:
                            "NIC lockdown status is unknown; a fresh observation is required".into(),
                    },
                    Err(_) => {
                        return Err(CarbideError::InvalidArgument(
                            "NIC firmware lockdown status is invalid".into(),
                        ));
                    }
                }
            }
            Some(AccessResult::Changed(_)) => NicFirmwareLockdownResult::Changed,
            Some(AccessResult::Error(diagnostic)) => {
                validate_diagnostic(diagnostic)?;
                NicFirmwareLockdownResult::Error {
                    diagnostic: diagnostic.clone(),
                }
            }
            None => {
                return Err(CarbideError::InvalidArgument(
                    "NIC firmware lockdown result is required".into(),
                ));
            }
        };
        if (!matches!(result, NicFirmwareLockdownResult::Error { .. })
            || matches!(
                lockdown.result.as_ref(),
                Some(AccessResult::StatusReport(_))
            ))
            && device_info.is_none()
        {
            return Err(CarbideError::InvalidArgument(
                "successful NIC firmware lockdown result requires observed device identity".into(),
            ));
        }
        Ok(Self {
            version: lockdown.controller_state_version.clone(),
            pci_name: lockdown.pci_name.clone(),
            expected_device,
            device_info,
            result,
        })
    }
}

impl FirmwareLockdown {
    /// Store a correlated result and return true. A later controller revision
    /// has consumed or superseded older evidence.
    /// Ignore it without overwriting the result awaiting the current iteration.
    fn apply(
        self,
        operation: &mut NicFirmwareOperation,
        version: config_version::ConfigVersion,
    ) -> CarbideResult<bool> {
        if self.version != version.to_string()
            || operation.desired_lockdown().is_none()
            || operation.lockdown.result.is_some()
        {
            return Ok(false);
        }
        let Some(target) = operation.current_target() else {
            return Ok(false);
        };
        if self.pci_name != target.pci_name || self.expected_device != target.selector {
            return Ok(false);
        }
        if self.device_info.as_ref().is_some_and(|device| {
            device.pci_name != target.pci_name || !target.selector.matches(device)
        }) {
            return Err(CarbideError::InvalidArgument(
                "NIC firmware lockdown result device identity contradicts its frozen target".into(),
            ));
        }
        if !matches!(
            (&self.result, operation.lockdown.step),
            (
                NicFirmwareLockdownResult::Status(_),
                NicFirmwareLockdownStep::Observe
            ) | (
                NicFirmwareLockdownResult::Changed,
                NicFirmwareLockdownStep::Change
            ) | (NicFirmwareLockdownResult::Error { .. }, _)
        ) {
            return Err(CarbideError::InvalidArgument(
                "NIC firmware lockdown result does not match the requested step".into(),
            ));
        }
        operation.lockdown.result = Some(self.result);
        Ok(true)
    }
}

impl TryFrom<&pb::MlxObservation> for FirmwareUpdate {
    type Error = CarbideError;

    fn try_from(observation: &pb::MlxObservation) -> CarbideResult<Self> {
        if observation.lock_status.is_some()
            || observation.profile_name.is_some()
            || observation.profile_synced.is_some()
            || observation.firmware_report.is_some()
            || observation.firmware_operation.is_some()
            || observation.lockdown_result.is_some()
        {
            return Err(CarbideError::InvalidArgument(
                "generic NIC firmware results cannot include legacy operation or lockdown fields"
                    .into(),
            ));
        }
        let update = observation.firmware_update_result.as_ref().ok_or_else(|| {
            CarbideError::InvalidArgument("NIC firmware update result is required".into())
        })?;
        validate_token(&update.operation_id)?;
        validate_token(&update.phase_id)?;
        let result = match update.result.as_ref() {
            Some(WireResult::Receipt(receipt)) => {
                if receipt.operation_id != update.operation_id {
                    return Err(CarbideError::InvalidArgument(
                        "NIC firmware receipt operation ID contradicts its result".into(),
                    ));
                }
                if let Some(diagnostic) = &receipt.diagnostic {
                    validate_diagnostic(diagnostic)?;
                }
                FirmwareUpdateResult::Receipt(
                    receipt
                        .clone()
                        .try_into()
                        .map_err(CarbideError::InvalidArgument)?,
                )
            }
            Some(WireResult::MissingReceipt(missing)) => {
                validate_token(&missing.supervisor_instance_id)?;
                FirmwareUpdateResult::MissingReceipt(missing.supervisor_instance_id.clone())
            }
            Some(WireResult::Error(error)) => {
                let code =
                    pb::MlxDeviceUpdateFirmwareErrorCode::try_from(error.code).map_err(|_| {
                        CarbideError::InvalidArgument("unknown firmware request error".into())
                    })?;
                let code = match code {
                    pb::MlxDeviceUpdateFirmwareErrorCode::InvalidTask => {
                        NicFirmwareDeliveryErrorCode::InvalidTask
                    }
                    pb::MlxDeviceUpdateFirmwareErrorCode::Conflict => {
                        NicFirmwareDeliveryErrorCode::Conflict
                    }
                    pb::MlxDeviceUpdateFirmwareErrorCode::Unsupported => {
                        NicFirmwareDeliveryErrorCode::Unsupported
                    }
                    pb::MlxDeviceUpdateFirmwareErrorCode::Unavailable => {
                        NicFirmwareDeliveryErrorCode::Unavailable
                    }
                    pb::MlxDeviceUpdateFirmwareErrorCode::Unspecified => {
                        return Err(CarbideError::InvalidArgument(
                            "firmware request error code is required".into(),
                        ));
                    }
                };
                validate_diagnostic(&error.diagnostic)?;
                FirmwareUpdateResult::Error(code, error.diagnostic.clone())
            }
            None => {
                return Err(CarbideError::InvalidArgument(
                    "firmware result is required".into(),
                ));
            }
        };
        Ok(Self {
            operation_id: update.operation_id.clone(),
            phase_id: update.phase_id.clone(),
            pci_name: update.pci_name.clone(),
            device_info: observation
                .device_info
                .clone()
                .map(TryInto::try_into)
                .transpose()
                .map_err(CarbideError::InvalidArgument)?,
            result,
        })
    }
}

/// Keep correlation identifiers within the supervisor wire contract.
fn validate_token(token: &str) -> CarbideResult<()> {
    if !(1..=256).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(CarbideError::InvalidArgument(
            "firmware identifiers must contain 1-256 printable nonspace ASCII bytes".into(),
        ));
    }
    Ok(())
}

/// Bound persisted diagnostic text before reading or changing machine state.
fn validate_diagnostic(diagnostic: &str) -> CarbideResult<()> {
    if diagnostic.is_empty() || diagnostic.len() > 2048 {
        return Err(CarbideError::InvalidArgument(
            "firmware diagnostic must contain 1-2048 UTF-8 bytes".into(),
        ));
    }
    Ok(())
}

/// Store authenticated generic results without consulting DPA feature flags.
/// Validate the whole generic batch before writing, then condition the state
/// change on its original revision so a delayed response cannot overwrite a
/// newer controller iteration. No hardware command is sent from ingestion.
pub(super) async fn persist(
    api: &Api,
    authenticated_machine_id: MachineId,
    report: &pb::MlxObservationReport,
) -> CarbideResult<()> {
    if report.machine_id != Some(authenticated_machine_id) {
        return Err(CarbideError::PermissionDeniedError(
            "firmware report machine ID does not match the authenticated host".into(),
        ));
    }
    let host_id = HostMachineId::try_from(authenticated_machine_id)?;
    let updates: Vec<_> = report
        .observations
        .iter()
        .filter(|observation| observation.firmware_update_result.is_some())
        .map(FirmwareUpdate::try_from)
        .collect::<CarbideResult<_>>()?;
    let access_results: Vec<_> = report
        .observations
        .iter()
        .filter(|observation| observation.lockdown_result.is_some())
        .map(FirmwareLockdown::try_from)
        .collect::<CarbideResult<_>>()?;
    // Bound this database attempt, not the firmware operation's lifetime.
    let persisted = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let machine = db::machine::find_one(api.pg_pool(), &host_id, Default::default())
            .await?
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "machine",
                id: host_id.to_string(),
            })?;
        let ManagedHostState::NicFirmwareUpdating { mut operation } =
            machine.current_state().clone()
        else {
            return Ok(false);
        };
        let mut changed = false;
        for result in access_results {
            changed |= result.apply(&mut operation, machine.current_version())?;
        }
        for update in updates {
            if let Some(target) = operation.current_target()
                && update.operation_id == operation.operation_id
                && update.phase_id == operation.operation_id
                && update.pci_name == target.pci_name
                && let Some(device) = &update.device_info
                && (!target.selector.matches(device) || device.pci_name != target.pci_name)
            {
                return Err(CarbideError::InvalidArgument(
                    "firmware result device identity contradicts its frozen target".into(),
                ));
            }
            changed |= match update.result {
                FirmwareUpdateResult::Receipt(receipt) => operation.accept_receipt(
                    &update.operation_id,
                    &update.phase_id,
                    &update.pci_name,
                    receipt,
                ),
                FirmwareUpdateResult::MissingReceipt(instance_id) => operation
                    .accept_missing_receipt(
                        &update.operation_id,
                        &update.phase_id,
                        &update.pci_name,
                        instance_id,
                    ),
                FirmwareUpdateResult::Error(code, diagnostic) => operation.accept_error(
                    &update.operation_id,
                    &update.phase_id,
                    &update.pci_name,
                    code,
                    diagnostic,
                ),
            };
        }
        if !changed {
            return Ok(false);
        }
        let version = machine.current_version();
        let state = ManagedHostState::NicFirmwareUpdating { operation };
        let mut txn = api.txn_begin().await?;
        match db::machine::try_update_controller_state(
            &mut txn,
            &host_id,
            version,
            version.increment(),
            &state,
        )
        .await?
        {
            ConditionalWrite::Applied(()) => txn.commit().await?,
            ConditionalWrite::NotApplied(_) => {
                return Err(CarbideError::FailedPrecondition(
                    "machine changed before the firmware report could be stored".into(),
                ));
            }
        }
        Ok(true)
    })
    .await
    .map_err(|_| {
        CarbideError::UnavailableError(
            "firmware report persistence exceeded its 30-second deadline; retry reporting".into(),
        )
    })??;
    // Polling can repeat settled or stale evidence indefinitely. Only a committed
    // change gives the controller new work; ignored reports need no wakeup.
    if !persisted {
        return Ok(());
    }
    if let Err(error) = api
        .machine_state_handler_enqueuer
        .enqueue_object(&host_id)
        .await
    {
        carbide_instrument::emit(StateHandlerWakeupFailed {
            trigger: WakeupTrigger::NicFirmwareObservation,
            machine_id: authenticated_machine_id,
            err: error.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use pb::mlx_device_lockdown_result::Result as AccessResult;

    use super::*;

    const DEVICE: &str = "0000:01:00.0";

    /// Delivery failures need a known code and bounded diagnostic before they
    /// can become operator-visible evidence in the persisted operation.
    #[test]
    fn firmware_error_decoder_preserves_valid_details_and_rejects_invalid_fields() {
        scenarios!(
            run = |(code, diagnostic): (i32, String)| {
                let observation = pb::MlxObservation {
                    firmware_update_result: Some(pb::MlxDeviceUpdateFirmwareResult {
                        operation_id: "nic-card-0".into(),
                        phase_id: "nic-phase-0".into(),
                        pci_name: DEVICE.into(),
                        result: Some(WireResult::Error(pb::MlxDeviceUpdateFirmwareError {
                            code,
                            diagnostic,
                        })),
                    }),
                    ..Default::default()
                };
                FirmwareUpdate::try_from(&observation).map(|update| {
                    let FirmwareUpdateResult::Error(code, diagnostic) = update.result else {
                        panic!("delivery error must remain an error result")
                    };
                    (code, diagnostic)
                }).map_err(drop)
            };
            "valid errors preserve their code and diagnostic" {
                (pb::MlxDeviceUpdateFirmwareErrorCode::Unavailable.into(), "helper unavailable".into())
                    => Yields((NicFirmwareDeliveryErrorCode::Unavailable, "helper unavailable".into())),
                (pb::MlxDeviceUpdateFirmwareErrorCode::InvalidTask.into(), "invalid request".into())
                    => Yields((NicFirmwareDeliveryErrorCode::InvalidTask, "invalid request".into())),
                (pb::MlxDeviceUpdateFirmwareErrorCode::Unsupported.into(), "unsupported request".into())
                    => Yields((NicFirmwareDeliveryErrorCode::Unsupported, "unsupported request".into())),
            }
            "unknown enum values cannot become meaningful failures" {
                (i32::MAX, "helper unavailable".into()) => Fails,
            }
            "the unspecified sentinel is not an error classification" {
                (pb::MlxDeviceUpdateFirmwareErrorCode::Unspecified.into(), "helper unavailable".into()) => Fails,
            }
            "diagnostics cannot exceed the persistence bound" {
                (pb::MlxDeviceUpdateFirmwareErrorCode::Unavailable.into(), "x".repeat(2049)) => Fails,
                (pb::MlxDeviceUpdateFirmwareErrorCode::Unavailable.into(), String::new()) => Fails,
            }
        );
    }

    /// One valid envelope makes each rejected field independently observable.
    fn access_observation(edit: impl FnOnce(&mut pb::MlxObservation)) -> pb::MlxObservation {
        let mut observation = pb::MlxObservation {
            device_info: Some(pb::MlxDeviceInfo {
                pci_name: DEVICE.into(),
                base_mac: "02:00:00:00:00:01".into(),
                ..Default::default()
            }),
            lockdown_result: Some(pb::MlxDeviceLockdownResult {
                controller_state_version: config_version::ConfigVersion::initial().to_string(),
                pci_name: DEVICE.into(),
                expected_device: Some(
                    MlxDeviceSelector::Mac("02:00:00:00:00:01".parse().unwrap()).into(),
                ),
                result: Some(AccessResult::StatusReport(pb::StatusReport {
                    device_id: DEVICE.into(),
                    status: pb::LockStatus::Unlocked.into(),
                    timestamp: "2026-10-27T12:00:00Z".into(),
                })),
            }),
            ..Default::default()
        };
        edit(&mut observation);
        observation
    }

    /// Decode usable typed evidence. Application correlation is checked below;
    /// authenticated integration tests prove persisted revision fencing.
    #[test]
    fn access_envelopes_preserve_result_kind_and_reject_incomplete_evidence() {
        scenarios!(
            run = |observation| FirmwareLockdown::try_from(&observation)
                .map(|lockdown| lockdown.result).map_err(drop);
            "observed status and mutation acknowledgment remain distinct" {
                access_observation(|_| {}) => Yields(NicFirmwareLockdownResult::Status(NicFirmwareLockdownStatus::Unlocked)),
                access_observation(|observation| {
                    observation.lockdown_result.as_mut().unwrap().result = Some(AccessResult::Changed(Default::default()));
                }) => Yields(NicFirmwareLockdownResult::Changed),
            }
            "a failed identity query can report failure without fabricating device facts" {
                access_observation(|observation| {
                    observation.device_info = None;
                    observation.lockdown_result.as_mut().unwrap().result = Some(AccessResult::Error("device identity unavailable".into()));
                }) => Yields(NicFirmwareLockdownResult::Error { diagnostic: "device identity unavailable".into() }),
            }
            "successful lockdown requires independently observed device identity" {
                access_observation(|observation| observation.device_info = None) => Fails,
            }
            "generic lockdown cannot be mixed with legacy or firmware evidence" {
                access_observation(|observation| observation.lock_status = Some(pb::LockStatus::Unlocked.into())) => Fails,
                access_observation(|observation| observation.firmware_update_result = Some(Default::default())) => Fails,
            }
            "an explicit unknown status remains retryable evidence" {
                access_observation(|observation| {
                    let Some(AccessResult::StatusReport(status)) = &mut observation.lockdown_result.as_mut().unwrap().result else { unreachable!() };
                    status.status = pb::LockStatus::Unknown.into();
                }) => Yields(NicFirmwareLockdownResult::Error { diagnostic: "NIC lockdown status is unknown; a fresh observation is required".into() }),
            }
            "unknown status still requires the observed card identity" {
                access_observation(|observation| {
                    observation.device_info = None;
                    let Some(AccessResult::StatusReport(status)) = &mut observation.lockdown_result.as_mut().unwrap().result else { unreachable!() };
                    status.status = pb::LockStatus::Unknown.into();
                }) => Fails,
            }
            "invalid status values are malformed rather than observed unknown" {
                access_observation(|observation| {
                    let Some(AccessResult::StatusReport(status)) = &mut observation.lockdown_result.as_mut().unwrap().result else { unreachable!() };
                    status.status = i32::MAX;
                }) => Fails,
            }
            "malformed timestamps cannot establish a status observation" {
                access_observation(|observation| {
                    let Some(AccessResult::StatusReport(status)) = &mut observation.lockdown_result.as_mut().unwrap().result else { unreachable!() };
                    status.timestamp = "invalid".into();
                }) => Fails,
            }
            "the status payload cannot name a different device from its envelope" {
                access_observation(|observation| {
                    let Some(AccessResult::StatusReport(status)) = &mut observation.lockdown_result.as_mut().unwrap().result else { unreachable!() };
                    status.device_id = "0000:02:00.0".into();
                }) => Fails,
            }
        );
    }

    // These guards belong to applying current evidence, after decoding. A
    // stale locator or selector is ignored; contradictory identity or a reply
    // to another step is malformed and must leave the operation untouched.
    #[test]
    fn lockdown_application_requires_the_current_card_and_requested_step() {
        type Edit = fn(&mut FirmwareLockdown);
        scenarios!(
            run = |edit: Edit| {
                let mut operation = model::test_support::nic_firmware::operation();
                operation.phase = NicFirmwarePhase::Unlocking { target_index: 0 };
                operation.targets[0].supervisor_instance_id = Some("helper".into());
                let version = config_version::ConfigVersion::initial();
                let mut result = FirmwareLockdown::try_from(&access_observation(|_| {})).unwrap();
                result.version = version.to_string();
                edit(&mut result);
                let before = operation.clone();
                let applied = result.apply(&mut operation, version).map_err(drop);
                if applied == Ok(true) {
                    assert_eq!(operation.lockdown.result,
                        Some(NicFirmwareLockdownResult::Status(NicFirmwareLockdownStatus::Unlocked)));
                } else {
                    assert_eq!(operation, before);
                }
                applied
            };
            "matching evidence is accepted" {
                (|_: &mut FirmwareLockdown| {}) as Edit => Yields(true),
            }
            "a current revision cannot correlate another locator or selector" {
                (|result: &mut FirmwareLockdown| result.pci_name = "0000:02:00.0".into()) as Edit => Yields(false),
                (|result: &mut FirmwareLockdown| result.expected_device = MlxDeviceSelector::Mac("02:00:00:00:00:ff".parse().unwrap())) as Edit => Yields(false),
            }
            "the independently observed card cannot contradict its envelope" {
                (|result: &mut FirmwareLockdown| result.device_info.as_mut().unwrap().pci_name = "0000:02:00.0".into()) as Edit => Fails,
                (|result: &mut FirmwareLockdown| result.device_info.as_mut().unwrap().base_mac = Some("02:00:00:00:00:ff".parse().unwrap())) as Edit => Fails,
            }
            "a mutation acknowledgement cannot answer a status read" {
                (|result: &mut FirmwareLockdown| result.result = NicFirmwareLockdownResult::Changed) as Edit => Fails,
            }
        );
    }

    #[test]
    fn firmware_envelopes_reject_mixed_missing_and_contradictory_execution() {
        type Edit = fn(&mut pb::MlxObservation);
        scenarios!(
            run = |edit: Edit| {
                let mut observation = pb::MlxObservation {
                    firmware_update_result: Some(pb::MlxDeviceUpdateFirmwareResult {
                        operation_id: "nic-workflow".into(), phase_id: "nic-workflow".into(),
                        pci_name: DEVICE.into(),
                        result: Some(WireResult::Receipt(pb::FirmwareOperation {
                            operation_id: "nic-workflow".into(),
                            state: pb::FirmwareOperationState::Running.into(),
                            report: Some(Default::default()),
                            ..Default::default()
                        })),
                    }),
                    ..Default::default()
                };
                edit(&mut observation);
                FirmwareUpdate::try_from(&observation).map(drop).map_err(drop)
            };
            "a correlated receipt is usable" {
                (|_: &mut pb::MlxObservation| {}) as Edit => Yields(()),
            }
            "legacy fields cannot supply meaning to a generic receipt" {
                (|observation: &mut pb::MlxObservation| observation.lock_status = Some(pb::LockStatus::Unlocked.into())) as Edit => Fails,
            }
            "a receipt must agree with its envelope" {
                (|observation: &mut pb::MlxObservation| {
                    let Some(WireResult::Receipt(receipt)) = &mut observation.firmware_update_result.as_mut().unwrap().result else { unreachable!() };
                    receipt.operation_id = "another-operation".into();
                }) as Edit => Fails,
            }
            "the result is required" {
                (|observation: &mut pb::MlxObservation| observation.firmware_update_result.as_mut().unwrap().result = None) as Edit => Fails,
            }
        );
    }

    // Host activation must not acquire per-card reset defaults or timings that
    // would change the supervisor's immutable request on a retry.
    #[test]
    fn host_reboot_keeps_reset_and_post_reset_timing_out_of_the_card_request() {
        let operation = model::test_support::nic_firmware::operation();
        let target = &operation.targets[0];
        let expected = pb::FirmwareFlasherProfile {
            firmware_spec: Some(target.resolution.entry.firmware.clone().into()),
            flash_spec: Some(pb::FlashSpec {
                firmware_url: "https://firmware.example.test/image.bin".into(),
                ..Default::default()
            }),
            flash_options: Some(pb::FlashOptions {
                reset: false,
                verify_image: false,
                verify_version: false,
                reset_level: 3,
                post_reset_verify_retry_interval_seconds: None,
                post_reset_verify_timeout_seconds: None,
            }),
        };
        assert_eq!(frozen_profile(&operation.options, target), expected);
        let observe = observe_request(&operation, target);
        assert_eq!(observe.operation_id, operation.operation_id);
        assert_eq!(observe.phase_id, operation.operation_id);
        assert_eq!(observe.profile, Some(expected));
        assert_eq!(
            observe.expected_digests,
            Some(pb::FirmwareArtifactDigests {
                image_sha256: "ab".repeat(32),
                configuration_sha256: None,
            })
        );
    }
}
