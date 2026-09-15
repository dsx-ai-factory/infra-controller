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

//! Switch certificate configuration via Component Manager.

use carbide_uuid::switch::SwitchId;
use component_manager::error::ComponentManagerError;
use model::component_manager::ConfigureSwitchCertificateState;
use model::switch::Switch;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SwitchStateHandlerContextObjects;
use crate::endpoint;

/// Whether certificate configuration runs during initial bring-up or maintenance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigureSwitchCertificateMode {
    /// During `Configuring`: skip when prerequisites are absent.
    BringUp,
    /// During `Maintenance`: fail when prerequisites are absent.
    Reconfigure,
}

/// Outcome of polling a switch certificate configuration job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigureSwitchCertificatePollOutcome {
    Completed,
    Failed(String),
    InProgress,
}

/// Result of attempting to start switch certificate configuration.
pub enum StartConfigureSwitchCertificateResult {
    JobStarted(String),
    EarlyTransition(StateHandlerOutcome<model::switch::SwitchControllerState>),
}

pub async fn start_configure_switch_certificate(
    switch_id: &SwitchId,
    state: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    domain_name: Option<&str>,
    mode: ConfigureSwitchCertificateMode,
) -> Result<StartConfigureSwitchCertificateResult, StateHandlerError> {
    if state.rack_id.is_none() {
        return Ok(match mode {
            ConfigureSwitchCertificateMode::BringUp => {
                tracing::info!(
                    switch_id = ?switch_id,
                    "Switch: no rack association, skipping certificate configuration",
                );
                StartConfigureSwitchCertificateResult::EarlyTransition(
                    StateHandlerOutcome::transition(
                        model::switch::SwitchControllerState::Configuring {
                            config_state: model::switch::ConfiguringState::RotateOsPassword,
                        },
                    ),
                )
            }
            ConfigureSwitchCertificateMode::Reconfigure => {
                StartConfigureSwitchCertificateResult::EarlyTransition(
                    StateHandlerOutcome::transition(model::switch::SwitchControllerState::Error {
                        cause: format!(
                            "Switch {switch_id} certificate reconfiguration: switch has no rack association"
                        ),
                    }),
                )
            }
        });
    }

    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return Ok(match mode {
            ConfigureSwitchCertificateMode::BringUp => {
                tracing::info!(
                    switch_id = ?switch_id,
                    "Switch: component manager is not configured, skipping certificate configuration",
                );
                StartConfigureSwitchCertificateResult::EarlyTransition(
                    StateHandlerOutcome::transition(
                        model::switch::SwitchControllerState::Configuring {
                            config_state: model::switch::ConfiguringState::RotateOsPassword,
                        },
                    ),
                )
            }
            ConfigureSwitchCertificateMode::Reconfigure => {
                StartConfigureSwitchCertificateResult::EarlyTransition(
                    StateHandlerOutcome::transition(model::switch::SwitchControllerState::Error {
                        cause: format!(
                            "Switch {switch_id} certificate reconfiguration: component manager not configured"
                        ),
                    }),
                )
            }
        });
    };

    if state.bmc_mac_address.is_none() {
        return Ok(StartConfigureSwitchCertificateResult::EarlyTransition(
            StateHandlerOutcome::transition(model::switch::SwitchControllerState::Error {
                cause: "No BMC MAC address on switch".to_string(),
            }),
        ));
    }

    let endpoint = match endpoint::resolve_switch_endpoint(
        switch_id,
        &ctx.services.db_pool,
        &ctx.services.credential_manager,
    )
    .await
    {
        Ok(endpoint) => endpoint,
        Err(error) => {
            return Ok(StartConfigureSwitchCertificateResult::EarlyTransition(
                StateHandlerOutcome::transition(model::switch::SwitchControllerState::Error {
                    cause: format!(
                        "Switch {switch_id}: cannot resolve switch endpoint for certificate configuration: {error}"
                    ),
                }),
            ));
        }
    };
    let job_id = component_manager
        .configure_switch_certificate(
            &endpoint,
            domain_name,
            Some(ctx.services.switch_mtls_services.as_slice()),
        )
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre::eyre!(
                "switch {:?}: failed to start switch certificate configuration: {}",
                switch_id,
                error
            ))
        })?;

    tracing::info!(
        %job_id,
        ?mode,
        switch_id = ?switch_id,
        "Switch: started switch certificate configuration",
    );

    Ok(StartConfigureSwitchCertificateResult::JobStarted(job_id))
}

pub async fn poll_configure_switch_certificate_job(
    switch_id: &SwitchId,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    job_id: &str,
) -> Result<ConfigureSwitchCertificatePollOutcome, StateHandlerError> {
    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return Ok(ConfigureSwitchCertificatePollOutcome::Failed(
            "component manager is not configured while waiting for switch certificate job"
                .to_string(),
        ));
    };

    let status = match component_manager
        .get_configure_switch_certificate_job_status(job_id)
        .await
    {
        Ok(status) => status,
        Err(error @ ComponentManagerError::NotFound(_)) => {
            return Ok(ConfigureSwitchCertificatePollOutcome::Failed(
                error.to_string(),
            ));
        }
        Err(error) => {
            return Err(StateHandlerError::GenericError(eyre::eyre!(
                "switch {:?}: failed to get switch certificate job status for {}: {}",
                switch_id,
                job_id,
                error
            )));
        }
    };

    Ok(match status.state {
        ConfigureSwitchCertificateState::Completed => {
            ConfigureSwitchCertificatePollOutcome::Completed
        }
        ConfigureSwitchCertificateState::Failed => ConfigureSwitchCertificatePollOutcome::Failed(
            status
                .error
                .unwrap_or_else(|| "switch certificate configuration failed".to_string()),
        ),
        ConfigureSwitchCertificateState::Started | ConfigureSwitchCertificateState::InProgress => {
            ConfigureSwitchCertificatePollOutcome::InProgress
        }
    })
}
