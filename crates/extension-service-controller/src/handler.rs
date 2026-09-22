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

//! State Handler implementation for Extension Services

use carbide_dpf::DpfError;
use carbide_machine_controller::dpf::DpfOperations;
use carbide_uuid::extension_service::ExtensionServiceId;
use model::extension_service::{
    DpfHelmChartServiceData, ExtensionService, ExtensionServiceLifecycleState,
};
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::ExtensionServiceStateHandlerContextObjects;
use crate::dpu_service::{
    dpu_service_mutable_patch, project_dpu_service, verify_dpu_service_owner_label,
    verify_dpu_service_ownership,
};

/// State handler for DPF Helm chart extension services.
#[derive(Debug, Default)]
pub struct ExtensionServiceStateHandler;

#[async_trait::async_trait]
impl StateHandler for ExtensionServiceStateHandler {
    type ObjectId = ExtensionServiceId;
    type State = ExtensionService;
    type ControllerState = ExtensionServiceLifecycleState;
    type ContextObjects = ExtensionServiceStateHandlerContextObjects;

    async fn handle_object_state(
        &self,
        service_id: &ExtensionServiceId,
        _state: &mut ExtensionService,
        controller_state: &ExtensionServiceLifecycleState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<ExtensionServiceLifecycleState>, StateHandlerError> {
        match controller_state {
            ExtensionServiceLifecycleState::Creating => reconcile_create(*service_id, ctx).await,
            ExtensionServiceLifecycleState::Updating => reconcile_update(*service_id, ctx).await,
            ExtensionServiceLifecycleState::Deleting => reconcile_delete(*service_id, ctx).await,
            // Terminal and converged states have no work. In particular, a
            // periodic scan of a terminal Deleted row must be harmless.
            ExtensionServiceLifecycleState::Ready
            | ExtensionServiceLifecycleState::Failed
            | ExtensionServiceLifecycleState::Deleted => Ok(StateHandlerOutcome::do_nothing()),
        }
    }
}

/// Reads the durable V1 desired state, then creates the corresponding detached
/// DPUService. The desired-state connection is released before any DPF call.
async fn reconcile_create(
    service_id: ExtensionServiceId,
    ctx: &mut StateHandlerContext<'_, ExtensionServiceStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ExtensionServiceLifecycleState>, StateHandlerError> {
    let version = {
        let mut connection = ctx.services.db_pool.acquire().await?;
        db::extension_service::find_version_info_of_known_service(&mut connection, service_id, None)
            .await?
    };
    let data = match DpfHelmChartServiceData::parse(&version.data) {
        Ok(data) => data,
        Err(error) => {
            return Ok(permanent_failure(
                service_id,
                "invalid desired Helm data",
                error,
            ));
        }
    };
    let service = project_dpu_service(service_id, carbide_dpf::NAMESPACE, &data);

    let Some(dpf_sdk) = ctx.services.dpf_sdk.as_ref() else {
        return Ok(StateHandlerOutcome::wait(
            "DPF SDK is unavailable; retrying DPUService creation".to_string(),
        ));
    };

    match dpf_sdk.create_dpu_service(&service).await {
        Ok(created) => {
            match verify_dpu_service_ownership(&created, service_id, carbide_dpf::NAMESPACE) {
                Ok(()) => Ok(StateHandlerOutcome::transition(
                    ExtensionServiceLifecycleState::Ready,
                )),
                Err(error) => Ok(permanent_failure(
                    service_id,
                    "created DPUService violates NICo ownership contract",
                    error,
                )),
            }
        }
        Err(error) if is_already_exists(&error) => {
            verify_existing_created_service(service_id, dpf_sdk.as_ref()).await
        }
        Err(error) if is_permanent_dpf_error(&error) => Ok(permanent_failure(
            service_id,
            "DPF rejected DPUService creation",
            error,
        )),
        Err(error) => {
            tracing::warn!(%service_id, error = %error, "DPF DPUService creation failed; will retry");
            Ok(StateHandlerOutcome::wait(
                "DPF DPUService creation failed; retrying".to_string(),
            ))
        }
    }
}

/// Treat a conflicting create as success only after reading the object and
/// validating the UUID-derived ownership and immutable identity contract.
async fn verify_existing_created_service(
    service_id: ExtensionServiceId,
    dpf_sdk: &dyn DpfOperations,
) -> Result<StateHandlerOutcome<ExtensionServiceLifecycleState>, StateHandlerError> {
    let expected_name = model::extension_service::DpfHelmChartIdentity::from_service_id(service_id)
        .dpu_service_name;
    match dpf_sdk.get_dpu_service(&expected_name).await {
        Ok(Some(existing)) => {
            match verify_dpu_service_ownership(&existing, service_id, carbide_dpf::NAMESPACE) {
                Ok(()) => Ok(StateHandlerOutcome::transition(
                    ExtensionServiceLifecycleState::Ready,
                )),
                Err(error) => Ok(permanent_failure(
                    service_id,
                    "existing DPUService violates NICo ownership contract",
                    error,
                )),
            }
        }
        // A fast create/delete race can make an AlreadyExists result briefly
        // unobservable. Keep Creating so periodic reconciliation retries it.
        Ok(None) => Ok(StateHandlerOutcome::wait(
            "DPF reported DPUService already exists but it is not readable yet; retrying"
                .to_string(),
        )),
        Err(error) if is_permanent_dpf_error(&error) => Ok(permanent_failure(
            service_id,
            "DPF rejected DPUService verification",
            error,
        )),
        Err(error) => {
            tracing::warn!(%service_id, error = %error, "could not verify existing DPUService; will retry");
            Ok(StateHandlerOutcome::wait(
                "could not verify existing DPUService; retrying".to_string(),
            ))
        }
    }
}

/// Reconciles a stable-V1 in-place Helm revision. The database connection used
/// to load the desired data is released before the DPF read or merge patch.
async fn reconcile_update(
    service_id: ExtensionServiceId,
    ctx: &mut StateHandlerContext<'_, ExtensionServiceStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ExtensionServiceLifecycleState>, StateHandlerError> {
    let version = {
        let mut connection = ctx.services.db_pool.acquire().await?;
        db::extension_service::find_version_info_of_known_service(&mut connection, service_id, None)
            .await?
    };
    let data = match DpfHelmChartServiceData::parse(&version.data) {
        Ok(data) => data,
        Err(error) => {
            return Ok(permanent_failure(
                service_id,
                "invalid desired Helm data",
                error,
            ));
        }
    };
    let service = project_dpu_service(service_id, carbide_dpf::NAMESPACE, &data);

    let Some(dpf_sdk) = ctx.services.dpf_sdk.as_ref() else {
        return Ok(StateHandlerOutcome::wait(
            "DPF SDK is unavailable; retrying DPUService update".to_string(),
        ));
    };

    let existing = match dpf_sdk.get_dpu_service(&service.name).await {
        Ok(Some(existing)) => existing,
        // A Ready service must already have a detached DPUService. Treat a
        // missing object as an ownership/lifecycle conflict rather than
        // recreating it from the update path.
        Ok(None) => {
            return Ok(permanent_failure(
                service_id,
                "DPUService is missing during update reconciliation",
                "DPUService not found",
            ));
        }
        Err(error) if is_permanent_dpf_error(&error) => {
            return Ok(permanent_failure(
                service_id,
                "DPF rejected DPUService update verification",
                error,
            ));
        }
        Err(error) => {
            tracing::warn!(%service_id, error = %error, "could not read DPUService before update; will retry");
            return Ok(StateHandlerOutcome::wait(
                "could not read DPUService before update; retrying".to_string(),
            ));
        }
    };

    if let Err(error) = verify_dpu_service_ownership(&existing, service_id, carbide_dpf::NAMESPACE)
    {
        return Ok(permanent_failure(
            service_id,
            "existing DPUService violates NICo ownership contract",
            error,
        ));
    }

    match dpf_sdk
        .patch_dpu_service(
            &service.name,
            dpu_service_mutable_patch(&service, existing.helm_chart.values.as_ref()),
        )
        .await
    {
        Ok(()) => Ok(StateHandlerOutcome::transition(
            ExtensionServiceLifecycleState::Ready,
        )),
        Err(error) if is_permanent_dpf_error(&error) => Ok(permanent_failure(
            service_id,
            "DPF rejected DPUService update",
            error,
        )),
        Err(error) => {
            tracing::warn!(%service_id, error = %error, "DPF DPUService update failed; will retry");
            Ok(StateHandlerOutcome::wait(
                "DPF DPUService update failed; retrying".to_string(),
            ))
        }
    }
}

/// Requests deletion of the UUID-derived DPUService, then keeps the durable
/// lifecycle in Deleting until DPF finalizers make the object disappear.
async fn reconcile_delete(
    service_id: ExtensionServiceId,
    ctx: &mut StateHandlerContext<'_, ExtensionServiceStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ExtensionServiceLifecycleState>, StateHandlerError> {
    let expected_name = model::extension_service::DpfHelmChartIdentity::from_service_id(service_id)
        .dpu_service_name;
    let Some(dpf_sdk) = ctx.services.dpf_sdk.as_ref() else {
        return Ok(StateHandlerOutcome::wait(
            "DPF SDK is unavailable; retrying DPUService deletion".to_string(),
        ));
    };

    let existing = match dpf_sdk.get_dpu_service(&expected_name).await {
        // DPF removes a DPUService asynchronously after its finalizers clean
        // up dependent Applications and resources. Absence is the only
        // confirmation that NICo's deletion is complete.
        Ok(None) => {
            return Ok(StateHandlerOutcome::transition(
                ExtensionServiceLifecycleState::Deleted,
            ));
        }
        Ok(Some(existing)) => existing,
        Err(error) if is_not_found(&error) => {
            return Ok(StateHandlerOutcome::transition(
                ExtensionServiceLifecycleState::Deleted,
            ));
        }
        Err(error) if is_permanent_dpf_error(&error) => {
            return Ok(permanent_failure(
                service_id,
                "DPF rejected DPUService deletion verification",
                error,
            ));
        }
        Err(error) => {
            tracing::warn!(%service_id, error = %error, "could not read DPUService before deletion; will retry");
            return Ok(StateHandlerOutcome::wait(
                "could not read DPUService before deletion; retrying".to_string(),
            ));
        }
    };

    if let Err(error) = verify_dpu_service_owner_label(&existing, service_id) {
        return Ok(permanent_failure(
            service_id,
            "existing DPUService violates NICo ownership contract",
            error,
        ));
    }

    // Kubernetes has already accepted deletion. DPF keeps the CR visible
    // while its finalizers remove dependent Applications and resources, so
    // poll rather than repeatedly issuing delete requests. `NotFound` above
    // remains the only completion signal.
    if existing.is_deleting {
        return Ok(StateHandlerOutcome::wait(
            "DPF DPUService finalization is in progress; retrying".to_string(),
        ));
    }

    match dpf_sdk.delete_dpu_service(&expected_name).await {
        // A successful Kubernetes delete only means deletion was accepted.
        // Keep polling until get returns NotFound after DPF finalizers finish.
        Ok(()) => Ok(StateHandlerOutcome::wait(
            "DPF accepted DPUService deletion; awaiting finalization".to_string(),
        )),
        Err(error) if is_not_found(&error) => Ok(StateHandlerOutcome::transition(
            ExtensionServiceLifecycleState::Deleted,
        )),
        Err(error) if is_permanent_dpf_error(&error) => Ok(permanent_failure(
            service_id,
            "DPF rejected DPUService deletion",
            error,
        )),
        Err(error) => {
            tracing::warn!(%service_id, error = %error, "DPF DPUService deletion failed; will retry");
            Ok(StateHandlerOutcome::wait(
                "DPF DPUService deletion failed; retrying".to_string(),
            ))
        }
    }
}

/// A permanent failure transitions the durable lifecycle to Failed. The
/// error text is logged, while the persisted lifecycle/outcome avoids chart
/// values and any potentially sensitive DPF server detail.
fn permanent_failure(
    service_id: ExtensionServiceId,
    operation: &'static str,
    error: impl std::fmt::Display,
) -> StateHandlerOutcome<ExtensionServiceLifecycleState> {
    tracing::warn!(%service_id, %error, operation, "DPF Helm chart reconciliation cannot proceed");
    StateHandlerOutcome::transition(ExtensionServiceLifecycleState::Failed)
}

fn is_already_exists(error: &DpfError) -> bool {
    matches!(error, DpfError::AlreadyExists { .. })
        || matches!(error, DpfError::KubeError(kube::Error::Api(status)) if status.is_already_exists() || status.is_conflict())
}

fn is_not_found(error: &DpfError) -> bool {
    matches!(error, DpfError::NotFound { .. })
        || matches!(error, DpfError::KubeError(kube::Error::Api(status)) if status.is_not_found())
}

/// Bad request and unprocessable-object failures cannot be repaired by
/// replaying an unchanged desired state, so they end the lifecycle.
fn is_permanent_dpf_error(error: &DpfError) -> bool {
    matches!(error, DpfError::ConfigError(_))
        || matches!(error, DpfError::KubeError(kube::Error::Api(status)) if matches!(status.code, 400 | 422))
}
