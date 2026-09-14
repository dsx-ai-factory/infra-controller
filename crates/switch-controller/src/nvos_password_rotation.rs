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

//! Restart-safe NVOS password rotation through component-manager.
//!
//! NICo stages each target and attempt before dispatch. A missing backend job
//! retries the same current-to-target transition. Completion writes and verifies
//! the per-device credential before promoting the matching attempt.

use std::sync::Arc;

use carbide_secrets::credentials::{CredentialKey, CredentialManager, Credentials};
use carbide_uuid::switch::SwitchId;
use component_manager::error::ComponentManagerError;
use component_manager::nv_switch_manager::{SwitchEndpoint, SwitchPasswordRotationState};
use db::ConditionalWrite::{Applied, NotApplied};
use db::credential_rotation::RotationAttemptNotEligible;
use model::switch::Switch;
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use crate::context::SwitchStateHandlerContextObjects;
use crate::endpoint;

/// Result of reconciling one switch against the current NVOS target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NvosPasswordRotationOutcome {
    /// No actionable controller work remains for this pass.
    UpToDate,

    /// Work remains blocked or in progress.
    Waiting(String),

    /// Expected-switch credential data prevents safe rotation.
    CredentialError(String),
}

/// Inputs shared by initial dispatch and completion persistence.
struct RotationInputs {
    endpoint: SwitchEndpoint,
    target_credentials: Credentials,
}

/// Classifies expected-switch NVOS credential fields without treating partial
/// or empty legacy rows as absent.
pub(crate) fn expected_nvos_credential_pair<'a>(
    username: Option<&'a str>,
    password: Option<&'a str>,
) -> Result<Option<(&'a str, &'a str)>, &'static str> {
    match (username, password) {
        (None, None) => Ok(None),
        (Some(""), Some(_)) => Err("Expected switch nvos_username must not be empty"),
        (Some(_), Some("")) => Err("Expected switch nvos_password must not be empty"),
        (Some(username), Some(password)) => Ok(Some((username, password))),
        _ => Err("Expected switch nvos_username and nvos_password must be set together"),
    }
}

/// Reads an immutable site credential directly from the configured writer.
pub(crate) async fn read_nvos_site_credentials(
    switch_id: &SwitchId,
    version: i32,
    credential_manager: &dyn CredentialManager,
) -> Result<Option<Credentials>, StateHandlerError> {
    let version = u32::try_from(version).map_err(|error| {
        StateHandlerError::GenericError(eyre::eyre!(
            "switch {switch_id}: invalid NVOS credential version {version}: {error}"
        ))
    })?;

    credential_manager
        .get_credentials_from_writer(&CredentialKey::switch_nvos_site_admin(version))
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "switch {switch_id}: failed to read NVOS credential version {version}: {error}"
            ))
        })
}

/// Resolves current endpoint credentials and the exact staged target password.
async fn rotation_inputs(
    switch_id: &SwitchId,
    target_version: i32,
    db_pool: &sqlx::PgPool,
    credential_manager: Arc<dyn CredentialManager>,
) -> Result<RotationInputs, StateHandlerError> {
    let endpoint = endpoint::resolve_switch_endpoint(switch_id, db_pool, &credential_manager)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "switch {switch_id}: could not resolve switch endpoint: {error}"
            ))
        })?;

    let target = read_nvos_site_credentials(switch_id, target_version, credential_manager.as_ref())
        .await?
        .ok_or_else(|| {
            StateHandlerError::GenericError(eyre::eyre!(
                "switch {switch_id}: missing NVOS target version {target_version}"
            ))
        })?;

    let Credentials::UsernamePassword {
        password: target_password,
        ..
    } = target;

    let Credentials::UsernamePassword { username, .. } = &endpoint.nvos_credentials;

    if username.is_empty() || target_password.is_empty() {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "switch {switch_id}: NVOS rotation requires a non-empty username and target password"
        )));
    }

    let target_credentials = Credentials::UsernamePassword {
        username: username.clone(),
        password: target_password,
    };

    Ok(RotationInputs {
        endpoint,
        target_credentials,
    })
}

/// Stores a completed target and promotes only its matching operation attempt.
///
/// Promotion requires both the configured writer and NICo's effective read
/// chain to return the target. This prevents a higher-priority secrets backend
/// from shadowing the new credential after the database records convergence.
async fn complete_staged_rotation(
    switch_id: &SwitchId,
    bmc_mac_address: mac_address::MacAddress,
    target_version: i32,
    expected_attempt: i32,
    expected_job_id: &str,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<NvosPasswordRotationOutcome, StateHandlerError> {
    let credential_manager = ctx.services.credential_manager.clone();

    let inputs = rotation_inputs(
        switch_id,
        target_version,
        &ctx.services.db_pool,
        credential_manager.clone(),
    )
    .await?;

    // The state controller serializes handlers for a switch. Complete credential
    // store I/O before opening the short transaction; the final attempt CAS still
    // rejects promotion if another worker changed the operation state.
    crate::configuring::persist_nvos_admin_credentials(
        switch_id,
        bmc_mac_address,
        &inputs.target_credentials,
        ctx,
    )
    .await?;

    let mut txn = ctx.services.db_pool.begin().await?;

    let completion = db::credential_rotation::record_device_rotation_succeeded(
        &mut txn,
        bmc_mac_address,
        db::credential_rotation::CredentialRotationType::Nvos,
        target_version,
        expected_attempt,
        expected_job_id,
    )
    .await?;

    txn.commit().await?;

    Ok(match completion {
        Applied(()) => NvosPasswordRotationOutcome::UpToDate,
        NotApplied(RotationAttemptNotEligible) => NvosPasswordRotationOutcome::Waiting(
            "NVOS operation changed while recording backend completion".to_string(),
        ),
    })
}

/// Asks component-manager to converge one already-claimed target attempt.
async fn ensure_staged_rotation(
    switch_id: &SwitchId,
    bmc_mac_address: mac_address::MacAddress,
    target_version: i32,
    expected_attempt: i32,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<NvosPasswordRotationOutcome, StateHandlerError> {
    let component_manager = ctx.services.component_manager.clone().ok_or_else(|| {
        StateHandlerError::GenericError(eyre::eyre!(
            "switch {switch_id}: component manager is unavailable for NVOS password submission"
        ))
    })?;

    let inputs = match rotation_inputs(
        switch_id,
        target_version,
        &ctx.services.db_pool,
        ctx.services.credential_manager.clone(),
    )
    .await
    {
        Ok(inputs) => inputs,
        Err(error) => {
            return Ok(NvosPasswordRotationOutcome::Waiting(format!(
                "NVOS password rotation inputs are not ready: {error}"
            )));
        }
    };

    let Credentials::UsernamePassword {
        password: target_password,
        ..
    } = &inputs.target_credentials;

    match component_manager
        .ensure_switch_password_rotation(&inputs.endpoint, target_password)
        .await
    {
        Ok(job_id) => {
            let mut txn = ctx.services.db_pool.begin().await?;

            let submission = db::credential_rotation::record_device_rotation_submitted(
                &mut txn,
                bmc_mac_address,
                db::credential_rotation::CredentialRotationType::Nvos,
                target_version,
                expected_attempt,
                &job_id,
            )
            .await?;

            txn.commit().await?;

            match submission {
                Applied(()) => Ok(NvosPasswordRotationOutcome::Waiting(format!(
                    "NVOS job {job_id} was submitted"
                ))),
                NotApplied(RotationAttemptNotEligible) => {
                    Err(StateHandlerError::GenericError(eyre::eyre!(
                        "switch {switch_id}: NVOS job returned after its staged attempt changed"
                    )))
                }
            }
        }
        Err(ComponentManagerError::OperationOutcomeUnknown(error)) => {
            Ok(NvosPasswordRotationOutcome::Waiting(format!(
                "NVOS submission outcome is unknown and will be retried: {error}"
            )))
        }
        Err(ComponentManagerError::RejectedBeforeDispatch(error)) => {
            let mut txn = ctx.services.db_pool.begin().await?;

            let failure = db::credential_rotation::record_device_rotation_failed(
                &mut txn,
                bmc_mac_address,
                db::credential_rotation::CredentialRotationType::Nvos,
                target_version,
                expected_attempt,
                "component-manager rejected the NVOS password request",
            )
            .await?;

            txn.commit().await?;

            match failure {
                Applied(()) => Err(StateHandlerError::GenericError(eyre::eyre!(
                    "switch {switch_id}: NVOS submission is not retryable without correction: \
                     {error}"
                ))),
                NotApplied(RotationAttemptNotEligible) => Ok(NvosPasswordRotationOutcome::Waiting(
                    "NVOS operation changed while recording failure".to_string(),
                )),
            }
        }
        Err(error) => Err(StateHandlerError::GenericError(eyre::eyre!(
            "switch {switch_id}: NVOS submission could not be attempted: {error}"
        ))),
    }
}

/// Starts another observation attempt for the same staged current-to-target
/// transition, then asks component-manager to converge it again.
async fn retry_staged_rotation(
    switch_id: &SwitchId,
    bmc_mac_address: mac_address::MacAddress,
    target_version: i32,
    expected_attempt: i32,
    expected_job_id: Option<&str>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<NvosPasswordRotationOutcome, StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;

    let attempt = db::credential_rotation::record_device_rotation_retry_started(
        &mut txn,
        bmc_mac_address,
        db::credential_rotation::CredentialRotationType::Nvos,
        target_version,
        expected_attempt,
        expected_job_id,
    )
    .await?;

    txn.commit().await?;

    match attempt {
        Applied(attempt) => {
            ensure_staged_rotation(switch_id, bmc_mac_address, target_version, attempt, ctx).await
        }
        NotApplied(RotationAttemptNotEligible) => Ok(NvosPasswordRotationOutcome::Waiting(
            "NVOS operation changed while retrying backend observation".to_string(),
        )),
    }
}

/// Polls work already staged before a prior dispatch.
async fn reconcile_staged_rotation(
    switch_id: &SwitchId,
    bmc_mac_address: mac_address::MacAddress,
    operation: &db::credential_rotation::DeviceRotationOperationState,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<NvosPasswordRotationOutcome, StateHandlerError> {
    let target_version = operation.rotating_to_version.ok_or_else(|| {
        StateHandlerError::GenericError(eyre::eyre!(
            "switch {switch_id}: staged NVOS operation has no target version"
        ))
    })?;

    if let Some(error) = &operation.rotate_last_error_redacted {
        return Ok(NvosPasswordRotationOutcome::Waiting(error.clone()));
    }

    let Some(job_id) = operation.rotate_job_id.as_deref() else {
        return retry_staged_rotation(
            switch_id,
            bmc_mac_address,
            target_version,
            operation.rotate_attempts,
            None,
            ctx,
        )
        .await;
    };

    let Some(component_manager) = ctx.services.component_manager.clone() else {
        return Ok(NvosPasswordRotationOutcome::Waiting(
            "component manager is unavailable while polling NVOS rotation".to_string(),
        ));
    };

    let status = match component_manager
        .get_switch_password_rotation_job_status(job_id)
        .await
    {
        Ok(status) => status,
        Err(error) => {
            return Ok(NvosPasswordRotationOutcome::Waiting(format!(
                "NVOS job {job_id} could not be observed and remains attached: {error}"
            )));
        }
    };

    match status {
        SwitchPasswordRotationState::Pending => Ok(NvosPasswordRotationOutcome::Waiting(format!(
            "NVOS job {job_id} is still in progress"
        ))),
        SwitchPasswordRotationState::Completed => {
            complete_staged_rotation(
                switch_id,
                bmc_mac_address,
                target_version,
                operation.rotate_attempts,
                job_id,
                ctx,
            )
            .await
        }
        SwitchPasswordRotationState::Failed
        | SwitchPasswordRotationState::NotFound
        | SwitchPasswordRotationState::Unknown => {
            retry_staged_rotation(
                switch_id,
                bmc_mac_address,
                target_version,
                operation.rotate_attempts,
                Some(job_id),
                ctx,
            )
            .await
        }
    }
}

/// Claims a published target before its first component-manager dispatch.
async fn stage_rotation(
    switch_id: &SwitchId,
    bmc_mac_address: mac_address::MacAddress,
    target_version: i32,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<NvosPasswordRotationOutcome, StateHandlerError> {
    rotation_inputs(
        switch_id,
        target_version,
        &ctx.services.db_pool,
        ctx.services.credential_manager.clone(),
    )
    .await?;

    let mut txn = ctx.services.db_pool.begin().await?;

    let attempt = db::credential_rotation::record_device_rotation_started(
        &mut txn,
        bmc_mac_address,
        db::credential_rotation::CredentialRotationType::Nvos,
        target_version,
    )
    .await?;

    txn.commit().await?;

    let Some(attempt) = attempt else {
        return Ok(NvosPasswordRotationOutcome::Waiting(
            "NVOS rotation could not be claimed because target or device state changed".to_string(),
        ));
    };

    ensure_staged_rotation(switch_id, bmc_mac_address, target_version, attempt, ctx).await
}

/// Checks that the per-device credential uses the confirmed password, when known.
async fn has_nvos_admin_credential(
    switch_id: &SwitchId,
    bmc_mac_address: mac_address::MacAddress,
    confirmed_version: Option<i32>,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<bool, StateHandlerError> {
    let current = ctx
        .services
        .credential_manager
        .get_credentials(&CredentialKey::SwitchNvosAdmin { bmc_mac_address })
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "switch {switch_id}: failed to read NVOS credentials for BMC MAC \
                 {bmc_mac_address}: {error}"
            ))
        })?;

    let Some(Credentials::UsernamePassword {
        username,
        password: current_password,
    }) = current
    else {
        return Ok(false);
    };

    if username.is_empty() || current_password.is_empty() {
        return Ok(false);
    }

    let Some(confirmed_version) = confirmed_version else {
        return Ok(true);
    };

    let Some(Credentials::UsernamePassword {
        password: confirmed_password,
        ..
    }) = read_nvos_site_credentials(
        switch_id,
        confirmed_version,
        ctx.services.credential_manager.as_ref(),
    )
    .await?
    else {
        return Ok(false);
    };

    Ok(current_password == confirmed_password)
}

/// Returns whether Ready should schedule an authoritative reconciliation pass.
///
/// This is only a preflight. Reconciliation reloads target and operation state
/// before changing credentials or dispatching a backend operation.
pub(crate) async fn needs_nvos_password_reconciliation(
    switch_id: &SwitchId,
    state: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<bool, StateHandlerError> {
    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return Ok(false);
    };

    if !component_manager.nv_switch.supports_password_rotation() {
        return Ok(false);
    }

    let Some(bmc_mac_address) = state.bmc_mac_address else {
        return Ok(false);
    };

    let mut conn = ctx.services.db_pool.acquire().await?;

    let Some(target_version) = db::credential_rotation::current_target_version(
        &mut conn,
        db::credential_rotation::CredentialRotationType::Nvos,
    )
    .await?
    else {
        return Ok(false);
    };

    let operation = db::credential_rotation::device_rotation_operation_state(
        &mut *conn,
        db::credential_rotation::CredentialRotationType::Nvos,
        bmc_mac_address,
    )
    .await?;

    // Credential reads may call an external store.
    drop(conn);

    if let Some(operation) = operation {
        if operation.rotate_last_error_redacted.is_some() && operation.rotate_job_id.is_none() {
            if matches!(
                operation.rotating_to_version,
                Some(staged_target) if staged_target < target_version
            ) {
                return Ok(true);
            }

            return Ok(!has_nvos_admin_credential(switch_id, bmc_mac_address, None, ctx).await?);
        }

        if operation.rotating_to_version.is_some() {
            return Ok(true);
        }

        let Some(current_version) = operation.current_version else {
            return Ok(true);
        };

        if current_version < target_version {
            return Ok(true);
        }

        return Ok(!has_nvos_admin_credential(
            switch_id,
            bmc_mac_address,
            Some(current_version),
            ctx,
        )
        .await?);
    }

    // Avoid cycling switches that have no known credential source.
    if has_nvos_admin_credential(switch_id, bmc_mac_address, None, ctx).await? {
        return Ok(true);
    }

    let mut conn = ctx.services.db_pool.acquire().await?;

    let expected_switch =
        db::expected_switch::find_by_bmc_mac_address(&mut conn, bmc_mac_address).await?;

    let Some(expected_switch) = expected_switch else {
        return Ok(false);
    };

    Ok(!matches!(
        expected_nvos_credential_pair(
            expected_switch.nvos_username.as_deref(),
            expected_switch.nvos_password.as_deref(),
        ),
        Ok(None)
    ))
}

/// Reconciles or submits NVOS password rotation work for one switch.
pub async fn reconcile_nvos_password_rotation(
    switch_id: &SwitchId,
    state: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<NvosPasswordRotationOutcome, StateHandlerError> {
    // Component-manager availability and password-rotation support are rollout
    // gates. The API cannot publish an NVOS target while either is absent, so
    // treat disabled rotation as a no-op and continue the switch lifecycle.
    // Durable rotation state remains unchanged and resumes when support returns.
    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return Ok(NvosPasswordRotationOutcome::UpToDate);
    };

    if !component_manager.nv_switch.supports_password_rotation() {
        return Ok(NvosPasswordRotationOutcome::UpToDate);
    }

    let mut conn = ctx.services.db_pool.acquire().await?;

    let Some(target_version) = db::credential_rotation::current_target_version(
        &mut conn,
        db::credential_rotation::CredentialRotationType::Nvos,
    )
    .await?
    else {
        return Ok(NvosPasswordRotationOutcome::UpToDate);
    };

    let Some(bmc_mac_address) = state.bmc_mac_address else {
        return Ok(NvosPasswordRotationOutcome::CredentialError(
            "No BMC MAC address on switch".to_string(),
        ));
    };

    // Credential resolution may call an external store. Release the database
    // connection before crossing that boundary.
    drop(conn);

    match crate::configuring::ensure_nvos_admin_credentials(switch_id, bmc_mac_address, ctx).await?
    {
        crate::configuring::NvosAdminCredentialStatus::Available => {}
        crate::configuring::NvosAdminCredentialStatus::Skip => {
            return Ok(NvosPasswordRotationOutcome::UpToDate);
        }
        crate::configuring::NvosAdminCredentialStatus::Error(cause) => {
            return Ok(NvosPasswordRotationOutcome::CredentialError(cause));
        }
    }

    let mut conn = ctx.services.db_pool.acquire().await?;

    let operation = db::credential_rotation::device_rotation_operation_state(
        &mut *conn,
        db::credential_rotation::CredentialRotationType::Nvos,
        bmc_mac_address,
    )
    .await?;

    drop(conn);

    if let Some(operation) = &operation
        && let Some(staged_target) = operation.rotating_to_version
    {
        if operation.rotate_last_error_redacted.is_some()
            && operation.rotate_job_id.is_none()
            && staged_target < target_version
        {
            return stage_rotation(switch_id, bmc_mac_address, target_version, ctx).await;
        }

        return reconcile_staged_rotation(switch_id, bmc_mac_address, operation, ctx).await;
    }

    if matches!(
        operation.as_ref().and_then(|state| state.current_version),
        Some(current_version) if current_version >= target_version
    ) {
        return Ok(NvosPasswordRotationOutcome::UpToDate);
    }

    stage_rotation(switch_id, bmc_mac_address, target_version, ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expected_nvos_credential_pair_distinguishes_absent_valid_and_invalid_rows() {
        let cases = [
            ((None, None), Ok(None)),
            (
                (Some("admin"), Some("password")),
                Ok(Some(("admin", "password"))),
            ),
            (
                (Some("admin"), None),
                Err("Expected switch nvos_username and nvos_password must be set together"),
            ),
            (
                (None, Some("password")),
                Err("Expected switch nvos_username and nvos_password must be set together"),
            ),
            (
                (Some(""), Some("password")),
                Err("Expected switch nvos_username must not be empty"),
            ),
            (
                (Some("admin"), Some("")),
                Err("Expected switch nvos_password must not be empty"),
            ),
        ];

        for ((username, password), expected) in cases {
            assert_eq!(expected_nvos_credential_pair(username, password), expected);
        }
    }
}
