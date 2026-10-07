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

//! Handler for SwitchControllerState::Configuring.

use carbide_secrets::credentials::{CredentialKey, Credentials};
use carbide_uuid::switch::SwitchId;
use model::switch::{ConfigureCertificateState, ConfiguringState, Switch, SwitchControllerState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::certificate::{
    ConfigureSwitchCertificateMode, StartConfigureSwitchCertificateResult,
    poll_configure_switch_certificate_job, start_configure_switch_certificate,
};
use crate::context::SwitchStateHandlerContextObjects;
use crate::nvos_password_rotation::{
    NvosPasswordRotationOutcome, expected_nvos_credential_pair, read_nvos_site_credentials,
    reconcile_nvos_password_rotation,
};

/// Result of resolving the current per-switch NVOS credential.
pub(crate) enum NvosAdminCredentialStatus {
    /// A credential is available to resolve the component-manager endpoint.
    Available,

    /// No credential source exists, so credential-dependent work is not actionable yet.
    Skip,

    /// Credential state required to resolve a usable credential is inconsistent.
    Error(String),
}

/// Handles the Configuring state for a switch.
pub async fn handle_configuring(
    switch_id: &SwitchId,
    state: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let config_state = match &state.controller_state.value {
        SwitchControllerState::Configuring { config_state } => config_state.clone(),
        _ => unreachable!("handle_configuring called with non-Configuring state"),
    };

    match config_state {
        ConfiguringState::ConfigureCertificate {
            configure_certificate,
        } => handle_configure_certificate(switch_id, state, ctx, configure_certificate).await,
        ConfiguringState::RotateOsPassword => {
            handle_rotate_os_password(switch_id, state, ctx).await
        }
    }
}

async fn handle_rotate_os_password(
    switch_id: &SwitchId,
    state: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    match reconcile_nvos_password_rotation(switch_id, state, ctx).await? {
        NvosPasswordRotationOutcome::UpToDate => Ok(StateHandlerOutcome::transition(
            SwitchControllerState::FetchInfo,
        )),
        NvosPasswordRotationOutcome::Waiting(reason) => Ok(StateHandlerOutcome::wait(reason)),
        NvosPasswordRotationOutcome::CredentialError(cause) => Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Error { cause },
        )),
    }
}

/// Persists and verifies the effective per-switch NVOS credential.
pub(crate) async fn persist_nvos_admin_credentials(
    switch_id: &SwitchId,
    bmc_mac_address: mac_address::MacAddress,
    credentials: &Credentials,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<(), StateHandlerError> {
    let key = CredentialKey::SwitchNvosAdmin { bmc_mac_address };

    ctx.services
        .credential_manager
        .set_credentials(&key, credentials)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "switch {switch_id}: failed to persist NVOS credentials: {error}"
            ))
        })?;

    let persisted = ctx
        .services
        .credential_manager
        .get_credentials_from_writer(&key)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "switch {switch_id}: failed to read back NVOS credentials: {error}"
            ))
        })?;

    if persisted.as_ref() != Some(credentials) {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "switch {switch_id}: NVOS credential writer readback did not match"
        )));
    }

    let effective = ctx
        .services
        .credential_manager
        .get_credentials(&key)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "switch {switch_id}: failed to read effective NVOS credentials: {error}"
            ))
        })?;

    if effective.as_ref() != Some(credentials) {
        return Err(StateHandlerError::GenericError(eyre::eyre!(
            "switch {switch_id}: NVOS credential is shadowed in the effective read chain"
        )));
    }

    Ok(())
}

/// Ensures the credential manager contains the switch's current NVOS credential.
///
/// Expected-switch credentials seed the initial value before rotation starts.
/// While the initial rotation is staged, the captured per-switch credential is
/// preserved. After convergence, it is repaired from the immutable confirmed
/// site credential.
pub(crate) async fn ensure_nvos_admin_credentials(
    switch_id: &SwitchId,
    bmc_mac_address: mac_address::MacAddress,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<NvosAdminCredentialStatus, StateHandlerError> {
    let key = CredentialKey::SwitchNvosAdmin { bmc_mac_address };

    // This effective value may be the bootstrap credential, the confirmed
    // credential, or a target persisted immediately before DB promotion.
    let existing = ctx
        .services
        .credential_manager
        .get_credentials(&key)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "switch {switch_id}: failed to read NVOS credentials from credential store: {error}"
            ))
        })?;

    let mut txn = ctx.services.db_pool.begin().await?;

    let expected_switch =
        db::expected_switch::find_by_bmc_mac_address(&mut txn, bmc_mac_address).await?;

    let operation = db::credential_rotation::device_rotation_operation_state(
        &mut *txn,
        db::credential_rotation::CredentialRotationType::Nvos,
        bmc_mac_address,
    )
    .await?;

    txn.commit().await?;

    // Rotation may resume after a restart. Before initial confirmation, retain
    // any valid per-switch credential. Also retain a staged target that was
    // persisted before its matching DB promotion completed.
    if let Some(operation) = operation.as_ref()
        && let Some(staged_version) = operation.rotating_to_version
    {
        let existing_password = match existing.as_ref() {
            Some(Credentials::UsernamePassword { username, password })
                if !username.is_empty() && !password.is_empty() =>
            {
                Some(password.as_str())
            }
            _ => None,
        };

        if operation.current_version.is_none() {
            // During initial rotation, no versioned credential is confirmed.
            // Preserve any valid per-switch credential so retries keep a
            // usable password.
            return Ok(if existing_password.is_some() {
                NvosAdminCredentialStatus::Available
            } else {
                NvosAdminCredentialStatus::Error(
                    "Staged NVOS rotation has no valid current per-switch credential".to_string(),
                )
            });
        }

        if let Some(existing_password) = existing_password
            && let Some(Credentials::UsernamePassword {
                password: staged_password,
                ..
            }) = read_nvos_site_credentials(
                switch_id,
                staged_version,
                ctx.services.credential_manager.as_ref(),
            )
            .await?
            && existing_password == staged_password
        {
            // Completion persists the target before promoting its DB version.
            // Preserve that target when recovering between those two steps.
            return Ok(NvosAdminCredentialStatus::Available);
        }
    }

    // Expected-switch data provides bootstrap credentials and the fallback
    // username because site-wide rotation targets contain only a password.
    let Some(expected_switch) = expected_switch else {
        return Ok(NvosAdminCredentialStatus::Error(format!(
            "No expected switch found for BMC MAC {bmc_mac_address}"
        )));
    };

    let expected_credentials = match expected_nvos_credential_pair(
        expected_switch.nvos_username.as_deref(),
        expected_switch.nvos_password.as_deref(),
    ) {
        Ok(None) => None,
        Ok(Some(credentials)) => Some(credentials),
        Err(error) => return Ok(NvosAdminCredentialStatus::Error(error.to_string())),
    };

    let current_version = operation
        .as_ref()
        .and_then(|operation| operation.current_version);

    let (credentials, source) = match current_version {
        Some(current_version) => {
            // A confirmed version is authoritative. Repair its password from
            // the immutable site credential instead of stale bootstrap data.
            let versioned = read_nvos_site_credentials(
                switch_id,
                current_version,
                ctx.services.credential_manager.as_ref(),
            )
            .await?
            .ok_or_else(|| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "switch {switch_id}: confirmed NVOS credential version {current_version} \
                     is missing"
                ))
            })?;

            let Credentials::UsernamePassword { password, .. } = versioned;

            if password.is_empty() {
                return Ok(NvosAdminCredentialStatus::Error(format!(
                    "Confirmed NVOS credential version {current_version} has an empty password"
                )));
            }

            // Rotation changes only the password. Preserve the established
            // per-switch username, or recover it from expected-switch data.
            let username = match existing.as_ref() {
                Some(Credentials::UsernamePassword { username, .. }) if !username.is_empty() => {
                    username.clone()
                }
                _ => {
                    let Some((username, _)) = expected_credentials else {
                        return Ok(NvosAdminCredentialStatus::Error(format!(
                            "Cannot restore confirmed NVOS credential version {current_version} \
                             without an expected-switch username"
                        )));
                    };

                    username.to_string()
                }
            };

            let credentials = Credentials::UsernamePassword { username, password };
            (credentials, "confirmed rotation version")
        }
        None => {
            // Before first convergence, expected-switch credentials bootstrap
            // the per-switch key.
            let Some((username, password)) = expected_credentials else {
                // Preserve an existing credential when bootstrap input is
                // absent so rotation can use it as the current credential.
                let Some(credentials) = existing else {
                    tracing::info!(
                        switch_id = ?switch_id,
                        bmc_mac_address = %bmc_mac_address,
                        "Switch: no NVOS credentials in credential store or expected switch; skipping",
                    );

                    return Ok(NvosAdminCredentialStatus::Skip);
                };

                persist_nvos_admin_credentials(switch_id, bmc_mac_address, &credentials, ctx)
                    .await?;

                return Ok(NvosAdminCredentialStatus::Available);
            };

            (
                Credentials::UsernamePassword {
                    username: username.to_string(),
                    password: password.to_string(),
                },
                "expected switch",
            )
        }
    };

    // Availability is reported only after writer and effective readback agree.
    persist_nvos_admin_credentials(switch_id, bmc_mac_address, &credentials, ctx).await?;

    tracing::info!(
        switch_id = ?switch_id,
        bmc_mac_address = %bmc_mac_address,
        source,
        "Switch: restored NVOS admin credentials",
    );

    Ok(NvosAdminCredentialStatus::Available)
}

async fn handle_configure_certificate(
    switch_id: &SwitchId,
    state: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    configure_certificate: ConfigureCertificateState,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    match configure_certificate {
        ConfigureCertificateState::Start => {
            handle_configure_certificate_start(switch_id, state, ctx).await
        }
        ConfigureCertificateState::WaitForComplete { job_id } => {
            handle_configure_certificate_wait_for_complete(switch_id, ctx, &job_id).await
        }
    }
}

async fn handle_configure_certificate_start(
    switch_id: &SwitchId,
    state: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    // `start_configure_switch_certificate` skips certificate bring-up without
    // either a rack association or component manager. Resolve credentials only
    // when that operation can start so skipped configurations retain their
    // existing behavior.
    //
    // Certificate endpoint construction requires a per-switch NVOS
    // username/password. Preserve an existing effective credential. When it is
    // absent, use the rotation resolver to seed expected-switch bootstrap data.
    if state.rack_id.is_some()
        && ctx.services.component_manager.is_some()
        && let Some(bmc_mac_address) = state.bmc_mac_address
    {
        let key = CredentialKey::SwitchNvosAdmin { bmc_mac_address };

        let credential_exists = ctx
            .services
            .credential_manager
            .get_credentials(&key)
            .await
            .map_err(|error| {
                StateHandlerError::GenericError(eyre::eyre!(
                    "switch {switch_id}: failed to read NVOS credentials from credential store: {error}"
                ))
            })?
            .is_some();

        if !credential_exists {
            match ensure_nvos_admin_credentials(switch_id, bmc_mac_address, ctx).await? {
                NvosAdminCredentialStatus::Available => {}
                NvosAdminCredentialStatus::Skip => {
                    // Stay in `ConfigureCertificate::Start` so periodic
                    // reconciliation retries after credentials are imported.
                    return Ok(StateHandlerOutcome::wait(format!(
                        "switch {switch_id}: waiting for NVOS admin credentials"
                    )));
                }
                NvosAdminCredentialStatus::Error(cause) => {
                    // Expected-switch credential metadata can be corrected
                    // without resetting the switch controller state.
                    return Ok(StateHandlerOutcome::wait(format!(
                        "switch {switch_id}: waiting for valid NVOS admin credentials: {cause}"
                    )));
                }
            }
        }
    }

    match start_configure_switch_certificate(
        switch_id,
        state,
        ctx,
        None,
        ConfigureSwitchCertificateMode::BringUp,
    )
    .await?
    {
        StartConfigureSwitchCertificateResult::EarlyTransition(outcome) => Ok(outcome),
        StartConfigureSwitchCertificateResult::JobStarted(job_id) => Ok(
            StateHandlerOutcome::transition(SwitchControllerState::Configuring {
                config_state: ConfiguringState::ConfigureCertificate {
                    configure_certificate: ConfigureCertificateState::WaitForComplete { job_id },
                },
            }),
        ),
    }
}

async fn handle_configure_certificate_wait_for_complete(
    switch_id: &SwitchId,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    job_id: &str,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    match poll_configure_switch_certificate_job(switch_id, ctx, job_id).await? {
        crate::certificate::ConfigureSwitchCertificatePollOutcome::Completed => {
            tracing::info!(
                %job_id,
                switch_id = ?switch_id,
                "Switch: switch certificate configuration completed",
            );
            Ok(StateHandlerOutcome::transition(
                SwitchControllerState::Configuring {
                    config_state: ConfiguringState::RotateOsPassword,
                },
            ))
        }
        crate::certificate::ConfigureSwitchCertificatePollOutcome::Failed(cause) => Ok(
            StateHandlerOutcome::transition(SwitchControllerState::Error { cause }),
        ),
        crate::certificate::ConfigureSwitchCertificatePollOutcome::InProgress => Ok(
            StateHandlerOutcome::wait(format!("switch certificate job {job_id} in progress")),
        ),
    }
}
