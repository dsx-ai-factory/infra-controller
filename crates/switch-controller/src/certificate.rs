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
use component_manager::config::{SwitchMtlsService, switch_mtls_services_as_i32};
use component_manager::error::ComponentManagerError;
use component_manager::nv_switch_manager::SwitchMtlsDisableState;
use model::component_manager::ConfigureSwitchCertificateState;
use model::switch::{ConfiguringState, Switch, SwitchControllerState};
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

/// Outcome of polling a switch mTLS-disable job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisableSwitchMtlsPollOutcome {
    Completed,
    Failed(String),
    InProgress,
}

/// Result of attempting to start switch mTLS removal.
pub enum StartDisableSwitchMtlsResult {
    JobStarted(String),

    /// Nothing to disable or the backend cannot disable; continue to certificate
    /// configuration.
    Skipped,

    EarlyTransition(StateHandlerOutcome<model::switch::SwitchControllerState>),
}

/// Result of attempting to start switch certificate configuration.
pub enum StartConfigureSwitchCertificateResult {
    JobStarted(String),
    EarlyTransition(StateHandlerOutcome<model::switch::SwitchControllerState>),
}

/// Returns the mTLS services that initial ingestion configures for `state`.
///
/// Certificate installation and mTLS removal share this selection so a disable
/// resets exactly the bindings that ingestion goes on to install.
fn switch_mtls_services_for(
    state: &Switch,
    ctx: &StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Vec<i32> {
    // Rack maintenance binds cluster applications after RMS selects the primary.
    let primary_only_services = switch_mtls_services_as_i32(&[
        SwitchMtlsService::ScaleUpFabricManager,
        SwitchMtlsService::ScaleUpFabricTelemetry,
    ]);

    ctx.services
        .switch_mtls_services
        .iter()
        .copied()
        .filter(|service| state.is_primary || !primary_only_services.contains(service))
        .collect()
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

    let services = switch_mtls_services_for(state, ctx);

    if services.is_empty() {
        tracing::info!(?switch_id, "No certificate services to configure");

        let next_state = match mode {
            ConfigureSwitchCertificateMode::BringUp => SwitchControllerState::Configuring {
                config_state: ConfiguringState::RotateOsPassword,
            },
            ConfigureSwitchCertificateMode::Reconfigure => SwitchControllerState::Ready,
        };

        return Ok(StartConfigureSwitchCertificateResult::EarlyTransition(
            StateHandlerOutcome::transition(next_state),
        ));
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
        .configure_switch_certificate(&endpoint, domain_name, Some(&services))
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

/// Submits removal of the switch's existing mTLS bindings.
///
/// Initial ingestion runs this before certificate installation because RMS must
/// reach NVUE with its client identity before it can install new material, and
/// stale client-CA trust makes NVUE reject that identity. RMS performs the
/// removal over SSH, which does not depend on NVUE.
///
/// Prerequisites absent for certificate configuration (rack association,
/// component manager, services) skip this step, since certificate
/// configuration then skips as well. A backend that does not support the
/// operation also skips it. An unknown dispatch outcome moves the switch to
/// `Error` without resubmitting.
pub async fn start_disable_switch_mtls(
    switch_id: &SwitchId,
    state: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StartDisableSwitchMtlsResult, StateHandlerError> {
    let services = switch_mtls_services_for(state, ctx);

    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return Ok(StartDisableSwitchMtlsResult::Skipped);
    };

    if state.rack_id.is_none() || services.is_empty() {
        return Ok(StartDisableSwitchMtlsResult::Skipped);
    }

    if state.bmc_mac_address.is_none() {
        return Ok(StartDisableSwitchMtlsResult::EarlyTransition(
            StateHandlerOutcome::transition(SwitchControllerState::Error {
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
            return Ok(StartDisableSwitchMtlsResult::EarlyTransition(
                StateHandlerOutcome::transition(SwitchControllerState::Error {
                    cause: format!(
                        "Switch {switch_id}: cannot resolve switch endpoint for disabling mTLS: {error}"
                    ),
                }),
            ));
        }
    };

    match component_manager
        .batch_disable_switch_mtls(std::slice::from_ref(&endpoint), &services)
        .await
    {
        Ok(job_id) => {
            tracing::info!(
                %job_id,
                switch_id = ?switch_id,
                "Switch: started disabling existing mTLS bindings",
            );
            Ok(StartDisableSwitchMtlsResult::JobStarted(job_id))
        }
        Err(ComponentManagerError::Unsupported(reason)) => {
            tracing::warn!(
                switch_id = ?switch_id,
                %reason,
                "Switch: disabling existing mTLS bindings is unsupported, skipping",
            );
            Ok(StartDisableSwitchMtlsResult::Skipped)
        }
        Err(error @ ComponentManagerError::OperationOutcomeUnknown(_)) => {
            // The request may have reached the backend. Do not resubmit.
            Ok(StartDisableSwitchMtlsResult::EarlyTransition(
                StateHandlerOutcome::transition(SwitchControllerState::Error {
                    cause: format!(
                        "Switch {switch_id}: disabling existing mTLS bindings has an unknown outcome; \
                         certificate configuration was not attempted: {error}"
                    ),
                }),
            ))
        }
        // Remaining errors occur before the backend accepts any work.
        Err(error) => Err(StateHandlerError::GenericError(eyre::eyre!(
            "switch {:?}: failed to start disabling mTLS: {}",
            switch_id,
            error
        ))),
    }
}

/// Polls a switch mTLS-disable job.
///
/// A job that cannot be observed or whose outcome is unknown fails the poll,
/// since certificate configuration must not start unless disable completed.
pub async fn poll_disable_switch_mtls_job(
    switch_id: &SwitchId,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
    job_id: &str,
) -> Result<DisableSwitchMtlsPollOutcome, StateHandlerError> {
    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return Ok(DisableSwitchMtlsPollOutcome::Failed(
            "component manager is not configured while waiting for switch mTLS-disable job"
                .to_string(),
        ));
    };

    let status = match component_manager
        .get_disable_switch_mtls_job_status(job_id)
        .await
    {
        Ok(status) => status,
        Err(
            error @ (ComponentManagerError::NotFound(_)
            | ComponentManagerError::OperationOutcomeUnknown(_)
            | ComponentManagerError::Unsupported(_)),
        ) => {
            return Ok(DisableSwitchMtlsPollOutcome::Failed(error.to_string()));
        }
        Err(error) => {
            return Err(StateHandlerError::GenericError(eyre::eyre!(
                "switch {:?}: failed to get switch mTLS-disable job status for {}: {}",
                switch_id,
                job_id,
                error
            )));
        }
    };

    Ok(match status.state {
        SwitchMtlsDisableState::Completed => DisableSwitchMtlsPollOutcome::Completed,
        SwitchMtlsDisableState::Failed => DisableSwitchMtlsPollOutcome::Failed(
            status
                .error
                .unwrap_or_else(|| "switch mTLS disable failed".to_string()),
        ),
        SwitchMtlsDisableState::Pending => DisableSwitchMtlsPollOutcome::InProgress,
    })
}
