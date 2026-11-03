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

use carbide_dpf::{
    DetachedDpfResourceObservation, DetachedDpuServiceChainDefinition,
    DetachedDpuServiceDefinition, DetachedDpuServiceInterfaceDefinition, DpfError,
    DpuServiceObservation, detached_dpu_service_chain_spec, detached_dpu_service_interface_spec,
};
use carbide_machine_controller::dpf::DpfOperations;
use carbide_uuid::extension_service::ExtensionServiceId;
use db::DatabaseError;
use futures::{StreamExt, stream};
use model::extension_service::{
    DPF_HELM_CHART_OWNER_LABEL, DpfHelmChartServiceData, ExtensionService,
    ExtensionServiceLifecycleState, ServiceVpcAddressFamily,
};
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::ExtensionServiceStateHandlerContextObjects;
use crate::dpu_service::{
    DpuServiceOwnershipConflict, ServiceVpcRegistrationResources, detached_resource_spec_patch,
    dpu_service_mutable_fields_match, dpu_service_mutable_patch, project_dpu_service,
    project_service_vpc_registration, service_chain_spec_matches, service_interface_spec_matches,
    verify_dpu_service_owner_label, verify_dpu_service_ownership, verify_service_chain_identity,
    verify_service_chain_ownership, verify_service_interface_identity,
    verify_service_interface_ownership,
};
use crate::interface_mac::ensure_interface_mac;

const DESIRED_PARENT_READ_CONCURRENCY: usize = 8;

/// Selects Creating, Updating, or Ready behavior after desired DPF resources converge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegistrationReconcileMode {
    Creating,
    Updating,
    Ready,
}

/// Holds durable DPF definitions and identity loaded from one consistent extension-service
/// snapshot.
struct DesiredRegistration {
    resources: ServiceVpcRegistrationResources,
    replacement_services: Vec<DetachedDpuServiceDefinition>,
    replacement_service_interfaces: Vec<DetachedDpuServiceInterfaceDefinition>,
    family: Option<ServiceVpcAddressFamily>,
    had_interface_mac: bool,
    has_references: bool,
    capacity_draining: bool,
}

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
        state: &mut ExtensionService,
        controller_state: &ExtensionServiceLifecycleState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<ExtensionServiceLifecycleState>, StateHandlerError> {
        match controller_state {
            ExtensionServiceLifecycleState::Creating => {
                reconcile_registration(*service_id, RegistrationReconcileMode::Creating, ctx).await
            }
            ExtensionServiceLifecycleState::Updating => {
                reconcile_registration(*service_id, RegistrationReconcileMode::Updating, ctx).await
            }
            ExtensionServiceLifecycleState::Deleting => reconcile_delete(*service_id, ctx).await,
            // Ready still reconciles the current fixed-slot inventory so capacity changes are
            // observable and repairable.
            ExtensionServiceLifecycleState::Ready => {
                // Return nonnetworked Ready services before strict V1 parsing to preserve
                // definitions created before service VPCs.
                if state.service_vpc_interfaces.is_empty() {
                    return Ok(StateHandlerOutcome::do_nothing());
                }
                reconcile_registration(*service_id, RegistrationReconcileMode::Ready, ctx).await
            }
            // Skip terminal rows to keep periodic scans of Deleted services harmless because they
            // have no external work.
            ExtensionServiceLifecycleState::Failed | ExtensionServiceLifecycleState::Deleted => {
                Ok(StateHandlerOutcome::do_nothing())
            }
        }
    }
}

/// Reconciles one registration from durable desired state through all dormant DPF parents, applying
/// the same sequence to Creating, Updating, and Ready so retries preserve partial progress.
async fn reconcile_registration(
    service_id: ExtensionServiceId,
    mode: RegistrationReconcileMode,
    ctx: &mut StateHandlerContext<'_, ExtensionServiceStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ExtensionServiceLifecycleState>, StateHandlerError> {
    // Load and commit the service row, V1 definition, references, and MAC before contacting DPF so
    // this pass cannot mix database versions.
    let desired = match load_desired_registration(
        service_id,
        mode,
        &ctx.services.db_pool,
        ctx.services.service_vpc.slots,
    )
    .await
    {
        Ok(desired) => desired,
        Err(LoadDesiredRegistrationError::InvalidData(error)) => {
            return Ok(permanent_failure(
                service_id,
                "invalid desired registration",
                error,
            ));
        }
        Err(LoadDesiredRegistrationError::Database(error)) => return Err(error.into()),
    };

    // A new deferred declaration has no prior topology to retire, so it must not contact DPF at
    // all.
    if mode == RegistrationReconcileMode::Creating
        && let Some(family) = desired.family
        && (!ctx.services.service_vpc.family_implemented(family)
            || !ctx.services.service_vpc.family_enabled(family))
    {
        let reason = if ctx.services.service_vpc.family_implemented(family) {
            "disabled"
        } else {
            "not implemented"
        };
        return Ok(StateHandlerOutcome::wait(format!(
            "service VPC {family:?} registration is {reason}; retrying after configuration rollout"
        )));
    }

    let Some(dpf_sdk) = ctx.services.dpf_sdk.as_ref() else {
        return Ok(StateHandlerOutcome::wait(
            "DPF SDK is unavailable; retrying extension-service registration".to_string(),
        ));
    };

    // Create unnetworked DPUService objects before reading them to preserve create-race ownership
    // checks without adding a GET to established Ready behavior.
    if mode == RegistrationReconcileMode::Creating
        && desired.family.is_none()
        && !desired.had_interface_mac
    {
        return Ok(create_or_verify_dpu_service(
            mode,
            service_id,
            &desired.resources.service,
            dpf_sdk.as_ref(),
            true,
        )
        .await);
    }

    let observed_service = match dpf_sdk
        .get_dpu_service(&desired.resources.service.name)
        .await
    {
        Ok(service) => service,
        Err(error) => {
            return Ok(dpf_failure_outcome(
                service_id,
                "could not read DPUService; retrying",
                error,
            ));
        }
    };

    // A topology replacement removes the DPUService first so it cannot retain a changing service
    // interface.
    if let Some(existing) = observed_service.as_ref() {
        if let Err(error) = verify_dpu_service_owner_label(existing, service_id) {
            return Ok(permanent_failure(
                service_id,
                "existing DPUService owner label or immutable fields do not match this registration",
                error,
            ));
        }
        if existing.is_deleting {
            // Ready is a promise about the complete external graph, so leave it before waiting.
            if mode == RegistrationReconcileMode::Ready {
                return Ok(StateHandlerOutcome::transition(
                    ExtensionServiceLifecycleState::Updating,
                ));
            }
            return Ok(StateHandlerOutcome::wait(
                "DPF DPUService finalization is in progress; retrying".to_string(),
            ));
        }
        if let Err(error) =
            verify_dpu_service_ownership(existing, service_id, &desired.resources.service)
        {
            let known_topology = desired
                .replacement_services
                .iter()
                .any(|source| verify_dpu_service_ownership(existing, service_id, source).is_ok());
            if !known_topology
                || !dpu_service_replacement_allowed(mode, desired.has_references, error)
            {
                return Ok(permanent_failure(
                    service_id,
                    "existing DPUService owner label or immutable fields do not match this registration",
                    error,
                ));
            }
            let preconditions = match existing.delete_preconditions() {
                Ok(preconditions) => preconditions,
                Err(error) => {
                    return Ok(permanent_failure(
                        service_id,
                        "DPUService deletion requires metadata.uid and metadata.resourceVersion",
                        error,
                    ));
                }
            };
            return Ok(
                match dpf_sdk
                    .delete_dpu_service(&desired.resources.service.name, &preconditions)
                    .await
                {
                    Ok(()) => StateHandlerOutcome::wait(format!(
                        "replacing DPUService after immutable contract change ({error}); awaiting finalization"
                    )),
                    Err(error) => dpf_failure_outcome(
                        service_id,
                        "DPUService replacement delete failed; retrying",
                        error,
                    ),
                },
            );
        }
    }

    // Publish durable Updating before repairing mutable DPUService drift, ensuring Ready continues
    // to mean every owned DPF resource has converged.
    if mode == RegistrationReconcileMode::Ready
        && observed_service.as_ref().is_some_and(|existing| {
            !dpu_service_mutable_fields_match(&desired.resources.service, existing)
        })
    {
        return Ok(StateHandlerOutcome::transition(
            ExtensionServiceLifecycleState::Updating,
        ));
    }

    // Missing desired state is repaired only after a durable non-Ready barrier is visible.
    if mode == RegistrationReconcileMode::Ready && observed_service.is_none() {
        return Ok(StateHandlerOutcome::transition(
            ExtensionServiceLifecycleState::Updating,
        ));
    }

    // Observe every dynamic parent before mutation so ownership conflicts and repair needs are
    // classified before any side effect.
    let observe_dynamic = desired.family.is_some() || desired.had_interface_mac;
    let owner_selector = format!("{DPF_HELM_CHART_OWNER_LABEL}={service_id}");
    let mut observed_chains = if observe_dynamic {
        match dpf_sdk
            .list_detached_dpu_service_chains(Some(&owner_selector))
            .await
        {
            Ok(chains) => chains,
            Err(error) => {
                return Ok(dpf_failure_outcome(
                    service_id,
                    "could not list service-VPC chains; retrying",
                    error,
                ));
            }
        }
    } else {
        Vec::new()
    };
    let mut observed_interfaces = if observe_dynamic {
        match dpf_sdk
            .list_detached_dpu_service_interfaces(Some(&owner_selector))
            .await
        {
            Ok(interfaces) => interfaces,
            Err(error) => {
                return Ok(dpf_failure_outcome(
                    service_id,
                    "could not list service-VPC interfaces; retrying",
                    error,
                ));
            }
        }
    } else {
        Vec::new()
    };

    // Bound exact-name interface reads to keep 32-slot registration within one handler budget while
    // completing every ownership check before mutation.
    let missing_interface_names = desired
        .resources
        .interfaces
        .iter()
        .filter(|interface| {
            !observed_interfaces
                .iter()
                .any(|existing| existing.name.as_deref() == Some(interface.name.as_str()))
        })
        .map(|interface| interface.name.clone())
        .collect::<Vec<_>>();
    let interface_reads = stream::iter(missing_interface_names)
        .map(|name| async move { dpf_sdk.get_detached_dpu_service_interface(&name).await })
        .buffered(DESIRED_PARENT_READ_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    for observation in interface_reads {
        match observation {
            Ok(Some(existing)) => observed_interfaces.push(existing),
            Ok(None) => {}
            Err(error) => {
                return Ok(dpf_failure_outcome(
                    service_id,
                    "could not read service-VPC interface; retrying",
                    error,
                ));
            }
        }
    }
    // Apply the same cap to chain reads while retaining deterministic error and observation order.
    let missing_chain_names = desired
        .resources
        .chains
        .iter()
        .filter(|chain| {
            !observed_chains
                .iter()
                .any(|existing| existing.name.as_deref() == Some(chain.name.as_str()))
        })
        .map(|chain| chain.name.clone())
        .collect::<Vec<_>>();
    let chain_reads = stream::iter(missing_chain_names)
        .map(|name| async move { dpf_sdk.get_detached_dpu_service_chain(&name).await })
        .buffered(DESIRED_PARENT_READ_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    for observation in chain_reads {
        match observation {
            Ok(Some(existing)) => observed_chains.push(existing),
            Ok(None) => {}
            Err(error) => {
                return Ok(dpf_failure_outcome(
                    service_id,
                    "could not read service-VPC chain; retrying",
                    error,
                ));
            }
        }
    }

    // A foreign object at an expected deterministic name is a conflict, never an adoption
    // opportunity.
    if foreign_expected_resource(
        service_id,
        &observed_interfaces,
        desired
            .resources
            .interfaces
            .iter()
            .map(|interface| interface.name.as_str()),
    ) {
        return Ok(permanent_failure(
            service_id,
            "DPUServiceInterface ownership label or immutable fields do not match this registration",
            DpuServiceOwnershipConflict::OwnershipLabel,
        ));
    }
    if foreign_expected_resource(
        service_id,
        &observed_chains,
        desired
            .resources
            .chains
            .iter()
            .map(|chain| chain.name.as_str()),
    ) {
        return Ok(permanent_failure(
            service_id,
            "DPUServiceChain ownership label or immutable fields do not match this registration",
            DpuServiceOwnershipConflict::OwnershipLabel,
        ));
    }

    // Only an exact prior-family attachment authorizes replacing immutable service-interface
    // identity.
    let replace_service_interface = if let Some(interface) = desired.resources.interfaces.first()
        && let Some(existing) = observed_interfaces
            .iter()
            .find(|existing| existing.name.as_deref() == Some(interface.name.as_str()))
        && !service_interface_spec_matches(existing, interface).unwrap_or(false)
        && desired.replacement_service_interfaces.iter().any(|source| {
            verify_service_interface_identity(existing, service_id, source).is_ok()
                && service_interface_spec_matches(existing, source) == Ok(true)
        }) {
        if observed_service.is_some()
            || mode != RegistrationReconcileMode::Updating
            || desired.has_references
        {
            return Ok(permanent_failure(
                service_id,
                "existing DPUServiceInterface cannot be replaced while its topology is active",
                "alternate-family service interface is still in use",
            ));
        }
        true
    } else {
        false
    };

    // Repair each parent spec only after its owner label, expected metadata, and any immutable
    // interface fields pass validation.
    let mut parent_spec_requires_repair = false;
    for interface in &desired.resources.interfaces {
        let Some(existing) = observed_interfaces
            .iter()
            .find(|existing| existing.name.as_deref() == Some(interface.name.as_str()))
        else {
            continue;
        };
        let replacing = replace_service_interface
            && desired
                .resources
                .interfaces
                .first()
                .map(|first| &first.name)
                == Some(&interface.name);
        if replacing {
            continue;
        }
        if let Err(error) = verify_service_interface_identity(existing, service_id, interface) {
            return Ok(permanent_failure(
                service_id,
                "existing DPUServiceInterface ownership label or immutable fields do not match this registration",
                error,
            ));
        }
        match service_interface_spec_matches(existing, interface) {
            Ok(matches) => parent_spec_requires_repair |= !matches,
            Err(error) => {
                return Ok(permanent_failure(
                    service_id,
                    "could not project desired DPUServiceInterface specification",
                    error,
                ));
            }
        }
    }
    for chain in &desired.resources.chains {
        let Some(existing) = observed_chains
            .iter()
            .find(|existing| existing.name.as_deref() == Some(chain.name.as_str()))
        else {
            continue;
        };
        if let Err(error) = verify_service_chain_identity(existing, service_id, chain) {
            return Ok(permanent_failure(
                service_id,
                "existing DPUServiceChain ownership label or immutable fields do not match this registration",
                error,
            ));
        }
        match service_chain_spec_matches(existing, chain) {
            Ok(matches) => parent_spec_requires_repair |= !matches,
            Err(error) => {
                return Ok(permanent_failure(
                    service_id,
                    "could not project desired DPUServiceChain specification",
                    error,
                ));
            }
        }
    }

    // Publish Updating before any repair side effect so the queued Updating pass owns every delete
    // and create required to restore the configured parent inventory.
    if mode == RegistrationReconcileMode::Ready
        && (parent_spec_requires_repair
            || resource_inventory_requires_repair(
                service_id,
                &observed_interfaces,
                desired
                    .resources
                    .interfaces
                    .iter()
                    .map(|interface| interface.name.as_str()),
            )
            || resource_inventory_requires_repair(
                service_id,
                &observed_chains,
                desired
                    .resources
                    .chains
                    .iter()
                    .map(|chain| chain.name.as_str()),
            ))
    {
        return Ok(StateHandlerOutcome::transition(
            ExtensionServiceLifecycleState::Updating,
        ));
    }

    // Chains depend on both interfaces, so obsolete or drifted chains finalize first.
    if let Some(outcome) = reconcile_obsolete_chains(
        service_id,
        dpf_sdk.as_ref(),
        &observed_chains,
        if replace_service_interface {
            &[]
        } else {
            &desired.resources.chains
        },
    )
    .await
    {
        return Ok(outcome);
    }
    if let Some(outcome) = reconcile_obsolete_interfaces(
        service_id,
        dpf_sdk.as_ref(),
        &observed_interfaces,
        &desired.resources.interfaces,
        replace_service_interface,
    )
    .await
    {
        return Ok(outcome);
    }

    // Updating may retire an old family before waiting, but it cannot create a deferred
    // replacement.
    if let Some(family) = desired.family
        && !ctx.services.service_vpc.family_implemented(family)
    {
        return Ok(StateHandlerOutcome::wait(format!(
            "service VPC {family:?} registration is not implemented; retrying after configuration rollout"
        )));
    }

    // Keep Ready registrations, referenced registrations, and registrations with an observed
    // DPUService repairable after family disablement.
    if let Some(family) = desired.family
        && !ctx.services.service_vpc.family_enabled(family)
        && mode != RegistrationReconcileMode::Ready
        && !desired.has_references
        && observed_service.is_none()
    {
        return Ok(StateHandlerOutcome::wait(format!(
            "service VPC {family:?} registration is disabled; retrying after configuration rollout"
        )));
    }

    // Create interfaces and chains first so the dormant DPUService never references incomplete
    // wiring. Remember partial progress so a later retryable failure schedules remaining parents
    // promptly.
    let mut parent_created = false;
    for interface in &desired.resources.interfaces {
        if observed_interfaces
            .iter()
            .any(|existing| existing.name.as_deref() == Some(interface.name.as_str()))
        {
            continue;
        }
        match dpf_sdk
            .create_detached_dpu_service_interface(interface)
            .await
        {
            Ok(created) => {
                if let Err(error) =
                    verify_service_interface_ownership(&created, service_id, interface)
                {
                    return Ok(permanent_failure(
                        service_id,
                        "created DPUServiceInterface ownership label or specification does not match this registration",
                        error,
                    ));
                }
                parent_created = true;
            }
            Err(error) if error.is_already_exists_or_conflict() => {
                // Reread only after POST conflict to distinguish an owned create race from a
                // foreign deterministic-name collision.
                let existing = match dpf_sdk
                    .get_detached_dpu_service_interface(&interface.name)
                    .await
                {
                    Ok(Some(existing)) => existing,
                    Ok(None) => {
                        return Ok(StateHandlerOutcome::wait(
                            "DPUServiceInterface create conflict disappeared before ownership verification; retrying"
                                .to_string(),
                        )
                        .requeue_if_waiting(parent_created));
                    }
                    Err(error) => {
                        return Ok(dpf_failure_outcome(
                            service_id,
                            "could not verify DPUServiceInterface after create conflict; retrying",
                            error,
                        )
                        .requeue_if_waiting(parent_created));
                    }
                };

                // Accept only the desired attachment or an exact prior-family attachment that
                // normal replacement can retire.
                let desired_identity =
                    verify_service_interface_identity(&existing, service_id, interface);
                let matches_prior_family =
                    desired.replacement_service_interfaces.iter().any(|source| {
                        verify_service_interface_identity(&existing, service_id, source).is_ok()
                            && service_interface_spec_matches(&existing, source) == Ok(true)
                    });
                if let Err(error) = &desired_identity
                    && !matches_prior_family
                {
                    return Ok(permanent_failure(
                        service_id,
                        "existing DPUServiceInterface ownership label or immutable fields do not match this registration",
                        error,
                    ));
                }
                if existing.is_deleting {
                    return Ok(StateHandlerOutcome::wait(
                        "DPUServiceInterface finalization is in progress; retrying".to_string(),
                    )
                    .requeue_if_waiting(parent_created));
                }
                if matches_prior_family {
                    return Ok(StateHandlerOutcome::wait_and_requeue(
                        "DPUServiceInterface create conflict requires topology replacement; retrying"
                            .to_string(),
                    ));
                }
                match service_interface_spec_matches(&existing, interface) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Ok(StateHandlerOutcome::wait_and_requeue(
                            "DPUServiceInterface create conflict requires mutable repair; retrying"
                                .to_string(),
                        ));
                    }
                    Err(error) => {
                        return Ok(permanent_failure(
                            service_id,
                            "could not project desired DPUServiceInterface specification",
                            error,
                        ));
                    }
                }
            }
            Err(error) => {
                return Ok(dpf_failure_outcome(
                    service_id,
                    "DPUServiceInterface creation failed; retrying",
                    error,
                )
                .requeue_if_waiting(parent_created));
            }
        }
    }
    for chain in &desired.resources.chains {
        if observed_chains
            .iter()
            .any(|existing| existing.name.as_deref() == Some(chain.name.as_str()))
        {
            continue;
        }
        match dpf_sdk.create_detached_dpu_service_chain(chain).await {
            Ok(created) => {
                if let Err(error) = verify_service_chain_ownership(&created, service_id, chain) {
                    return Ok(permanent_failure(
                        service_id,
                        "created DPUServiceChain ownership label or specification does not match this registration",
                        error,
                    ));
                }
                parent_created = true;
            }
            Err(error) if error.is_already_exists_or_conflict() => {
                // Reread only after POST conflict to distinguish an owned create race from a
                // foreign deterministic-name collision.
                let existing = match dpf_sdk.get_detached_dpu_service_chain(&chain.name).await {
                    Ok(Some(existing)) => existing,
                    Ok(None) => {
                        return Ok(StateHandlerOutcome::wait(
                            "DPUServiceChain create conflict disappeared before ownership verification; retrying"
                                .to_string(),
                        )
                        .requeue_if_waiting(parent_created));
                    }
                    Err(error) => {
                        return Ok(dpf_failure_outcome(
                            service_id,
                            "could not verify DPUServiceChain after create conflict; retrying",
                            error,
                        )
                        .requeue_if_waiting(parent_created));
                    }
                };
                if let Err(error) = verify_service_chain_identity(&existing, service_id, chain) {
                    return Ok(permanent_failure(
                        service_id,
                        "existing DPUServiceChain ownership label or immutable fields do not match this registration",
                        error,
                    ));
                }
                if existing.is_deleting {
                    return Ok(StateHandlerOutcome::wait(
                        "DPUServiceChain finalization is in progress; retrying".to_string(),
                    )
                    .requeue_if_waiting(parent_created));
                }
                match service_chain_spec_matches(&existing, chain) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Ok(StateHandlerOutcome::wait_and_requeue(
                            "DPUServiceChain create conflict requires mutable repair; retrying"
                                .to_string(),
                        ));
                    }
                    Err(error) => {
                        return Ok(permanent_failure(
                            service_id,
                            "could not project desired DPUServiceChain specification",
                            error,
                        ));
                    }
                }
            }
            Err(error) => {
                return Ok(dpf_failure_outcome(
                    service_id,
                    "DPUServiceChain creation failed; retrying",
                    error,
                )
                .requeue_if_waiting(parent_created));
            }
        }
    }

    // Reconcile the DPUService only after all parents exist because a matching consumer completes
    // the dormant resource set required for Ready.
    let Some(existing) = observed_service.as_ref() else {
        return Ok(create_or_verify_dpu_service(
            mode,
            service_id,
            &desired.resources.service,
            dpf_sdk.as_ref(),
            !desired.capacity_draining,
        )
        .await
        .requeue_if_waiting(parent_created));
    };

    Ok(reconcile_observed_dpu_service(
        mode,
        service_id,
        &desired.resources.service,
        existing,
        dpf_sdk.as_ref(),
        !desired.capacity_draining,
    )
    .await)
}

/// Creates the desired DPUService or verifies the object that won a same-name create race.
/// Treating an ownership-checked conflict as convergence keeps retries idempotent without adopting
/// foreign state.
async fn create_or_verify_dpu_service(
    mode: RegistrationReconcileMode,
    service_id: ExtensionServiceId,
    desired: &DetachedDpuServiceDefinition,
    dpf_sdk: &dyn DpfOperations,
    ready_allowed: bool,
) -> StateHandlerOutcome<ExtensionServiceLifecycleState> {
    match dpf_sdk.create_dpu_service(desired).await {
        Ok(created) => match verify_dpu_service_ownership(&created, service_id, desired) {
            Ok(()) => {
                reconcile_observed_dpu_service(
                    mode,
                    service_id,
                    desired,
                    &created,
                    dpf_sdk,
                    ready_allowed,
                )
                .await
            }
            Err(error) => permanent_failure(
                service_id,
                "created DPUService ownership label or immutable fields do not match this registration",
                error,
            ),
        },
        Err(error) if error.is_already_exists_or_conflict() => match dpf_sdk
            .get_dpu_service(&desired.name)
            .await
        {
            Ok(Some(existing)) => {
                match verify_dpu_service_ownership(&existing, service_id, desired) {
                    // An owned same-name object cannot converge while Kubernetes is finalizing it.
                    Ok(()) if existing.is_deleting => StateHandlerOutcome::wait(
                        "DPF DPUService finalization is in progress; retrying".to_string(),
                    ),
                    Ok(()) => {
                        reconcile_observed_dpu_service(
                            mode,
                            service_id,
                            desired,
                            &existing,
                            dpf_sdk,
                            ready_allowed,
                        )
                        .await
                    }
                    Err(error) => permanent_failure(
                        service_id,
                        "existing DPUService owner label or immutable fields do not match this registration",
                        error,
                    ),
                }
            }
            Ok(None) => StateHandlerOutcome::wait(
                "DPUService create conflict disappeared before ownership verification; retrying"
                    .to_string(),
            ),
            Err(error) => dpf_failure_outcome(
                service_id,
                "could not verify DPUService after create conflict; retrying",
                error,
            ),
        },
        Err(error) => dpf_failure_outcome(
            service_id,
            "DPF DPUService creation failed; retrying",
            error,
        ),
    }
}

/// Reconciles mutable DPUService state from an ownership-validated observation.
/// Separating discovery from convergence prevents Creating from publishing Ready for mutable drift.
async fn reconcile_observed_dpu_service(
    mode: RegistrationReconcileMode,
    service_id: ExtensionServiceId,
    desired: &DetachedDpuServiceDefinition,
    existing: &DpuServiceObservation,
    dpf_sdk: &dyn DpfOperations,
    ready_allowed: bool,
) -> StateHandlerOutcome<ExtensionServiceLifecycleState> {
    if dpu_service_mutable_fields_match(desired, existing) {
        return converged_dpu_service_outcome(mode, ready_allowed);
    }

    // A durable Updating barrier makes mutable repair visible before its side effect.
    if mode != RegistrationReconcileMode::Updating {
        return StateHandlerOutcome::transition(ExtensionServiceLifecycleState::Updating);
    }

    // Fence the patch with the resource version from the validated DPUService to avoid another
    // Kubernetes GET.
    let Some(resource_version) = existing.resource_version.as_deref() else {
        return permanent_failure(
            service_id,
            "DPUService update requires metadata.resourceVersion",
            "observed DPUService is missing metadata.resourceVersion",
        );
    };
    let patch = dpu_service_mutable_patch(desired, existing, resource_version);
    match dpf_sdk.patch_dpu_service(&desired.name, patch).await {
        Ok(patched) => match verify_dpu_service_ownership(&patched, service_id, desired) {
            Ok(()) if patched.is_deleting => StateHandlerOutcome::wait(
                "DPF DPUService finalization began during update; retrying".to_string(),
            ),
            Ok(()) if dpu_service_mutable_fields_match(desired, &patched) => {
                converged_dpu_service_outcome(mode, ready_allowed)
            }
            Ok(()) => StateHandlerOutcome::wait(
                "DPUService update response has not converged; retrying".to_string(),
            ),
            Err(error) => permanent_failure(
                service_id,
                "updated DPUService ownership label or immutable fields do not match this registration",
                error,
            ),
        },
        Err(error) => dpf_failure_outcome(service_id, "DPUService update failed; retrying", error),
    }
}

/// Returns the lifecycle outcome for a converged DPUService while retaining higher-slot
/// DPUServiceInterface and DPUServiceChain resources during a capacity drain.
fn converged_dpu_service_outcome(
    mode: RegistrationReconcileMode,
    ready_allowed: bool,
) -> StateHandlerOutcome<ExtensionServiceLifecycleState> {
    // A reduced slot count cannot become Ready while persisted service interfaces still require
    // higher-slot DPUServiceInterface and DPUServiceChain resources.
    if !ready_allowed {
        return match mode {
            // Leave Ready so later reconciliation cannot delete higher-slot
            // DPUServiceInterface and DPUServiceChain resources.
            RegistrationReconcileMode::Ready => {
                StateHandlerOutcome::transition(ExtensionServiceLifecycleState::Updating)
            }
            // Creating and Updating already block deletion, so wait for service-interface reclamation.
            RegistrationReconcileMode::Creating | RegistrationReconcileMode::Updating => {
                StateHandlerOutcome::wait(
                    "service-VPC slot capacity is draining retained endpoints; retrying"
                        .to_string(),
                )
            }
        };
    }

    // Creating and Updating complete only after every owned field converges.
    match mode {
        RegistrationReconcileMode::Creating | RegistrationReconcileMode::Updating => {
            StateHandlerOutcome::transition(ExtensionServiceLifecycleState::Ready)
        }
        RegistrationReconcileMode::Ready => StateHandlerOutcome::do_nothing(),
    }
}

/// Distinguishes malformed durable desired state from retryable database failures.
/// The controller persists malformed input as Failed without leaking its potentially sensitive
/// contents.
enum LoadDesiredRegistrationError {
    InvalidData(String),
    Database(DatabaseError),
}

impl From<DatabaseError> for LoadDesiredRegistrationError {
    /// Preserves database failure classification while the loader adds desired-state parsing
    /// errors.
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

/// Extends configured slot inventory through the highest persisted service-interface `slot_index`
/// and reports whether the registration must remain non-Ready until reclamation.
fn effective_service_vpc_slots(
    configured: carbide_dpf::ServiceVpcSlots,
    highest_retained_slot: Option<u32>,
) -> Result<(carbide_dpf::ServiceVpcSlots, bool), DpfError> {
    let retained_count = highest_retained_slot
        .map(|slot| {
            slot.checked_add(1).ok_or_else(|| {
                DpfError::ConfigError(format!(
                    "retained service-VPC slot {slot} cannot be represented as a slot count"
                ))
            })
        })
        .transpose()?
        .unwrap_or_default();
    Ok((
        carbide_dpf::ServiceVpcSlots::new(configured.len().max(retained_count))?,
        retained_count > configured.len(),
    ))
}

/// Loads the stable V1 definition, attachment state, and MAC identity under the service row lock.
/// This serializes snapshot construction and MAC allocation with database attachment and update
/// writers.
/// The transaction commits before Kubernetes reconciliation.
async fn load_desired_registration(
    service_id: ExtensionServiceId,
    mode: RegistrationReconcileMode,
    db_pool: &sqlx::PgPool,
    service_vpc_slots: carbide_dpf::ServiceVpcSlots,
) -> Result<DesiredRegistration, LoadDesiredRegistrationError> {
    let mut txn = db::Transaction::begin(db_pool).await?;

    // Re-read under lock because the state-controller snapshot predates this reconciliation call.
    let service = db::extension_service::find_by_ids(txn.as_mut(), &[service_id], true, true)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "extension_service",
            id: service_id.to_string(),
        })?;
    let version =
        db::extension_service::find_version_info_of_known_service(txn.as_mut(), service_id, None)
            .await?;
    let data = DpfHelmChartServiceData::parse(&version.data)
        .map_err(|error| LoadDesiredRegistrationError::InvalidData(error.to_string()))?;
    // Skip identity and reference revalidation during Ready scans to rebuild accepted DPF
    // definitions without population-wide database work each period.
    let has_references = if mode == RegistrationReconcileMode::Ready {
        false
    } else {
        db::extension_service::is_service_in_use(txn.as_mut(), service_id, &[], true).await?
    };
    let existing_macs =
        db::extension_service::find_interface_macs(txn.as_mut(), service_id).await?;
    let had_interface_mac = !existing_macs.is_empty();

    // Retain slot-specific DPUServiceInterface and DPUServiceChain resources above a reduced
    // configured count until every endpoint using them is gone. This lookup intentionally runs
    // during Ready reconciliation because persisted endpoints may be the only evidence that a
    // higher-slot resource must be repaired rather than deleted. Simply ignoring higher observed
    // resources would leave them outside normal desired reconciliation (operationally orphaned
    // until owner-labelled service deletion discovers them) and could leave them pointing at fixed
    // slot resources removed during DPU reprovisioning. Retaining every higher-slot resource
    // indefinitely would avoid the database scan but prevent convergence to a reduced inventory.
    // A later count increase safely recreates released resources from their deterministic names.
    // Replace this scan only with durable retained-slot indexing or an explicit configuration
    // contract that rejects slot-count reductions.
    let highest_retained_slot =
        db::instance::highest_retained_service_vpc_slot(&mut txn, service_id).await?;
    let (effective_service_vpc_slots, capacity_draining) =
        effective_service_vpc_slots(service_vpc_slots, highest_retained_slot)
            .map_err(|error| LoadDesiredRegistrationError::InvalidData(error.to_string()))?;

    // The MVP admits one ordinal; persistence and allocation still retain the ordinal explicitly.
    if service.service_vpc_interfaces.len() > 1 {
        return Err(LoadDesiredRegistrationError::InvalidData(
            "at most one service VPC interface is supported".to_string(),
        ));
    }
    let network_identity = match service.service_vpc_interfaces.first().copied() {
        Some(requirement) => Some((
            requirement.address_family,
            ensure_interface_mac(txn.as_mut(), service_id, 0).await?,
        )),
        None => None,
    };
    // Commit any newly allocated MAC before returning DPF resource definitions so every Kubernetes
    // retry uses the same Ethernet address.
    txn.commit().await?;

    // Build DPF resource definitions after releasing the service row lock so Kubernetes
    // reconciliation never holds the database transaction open.
    let family = network_identity.map(|(family, _)| family);
    let resources = match network_identity {
        Some((family, mac)) => project_service_vpc_registration(
            service_id,
            carbide_dpf::NAMESPACE,
            &data,
            family,
            mac,
            effective_service_vpc_slots,
        )
        .map_err(|error| LoadDesiredRegistrationError::InvalidData(error.to_string()))?,
        None => ServiceVpcRegistrationResources {
            service: project_dpu_service(
                service_id,
                carbide_dpf::NAMESPACE,
                &data,
                service.deployment_type,
            ),
            interfaces: Vec::new(),
            chains: Vec::new(),
        },
    };

    // Replacement accepts only another complete NICo topology, never arbitrary immutable drift.
    let retained_mac = network_identity.map(|(_, mac)| mac).or_else(|| {
        existing_macs
            .iter()
            .find(|assignment| assignment.interface_ordinal == 0)
            .copied()
    });
    let mut replacement_services = Vec::new();
    let mut replacement_service_interfaces = Vec::new();
    if family.is_some() {
        replacement_services.push(project_dpu_service(
            service_id,
            carbide_dpf::NAMESPACE,
            &data,
            service.deployment_type,
        ));
    }
    if let Some(mac) = retained_mac {
        for alternate_family in [ServiceVpcAddressFamily::Ipv4, ServiceVpcAddressFamily::Ipv6]
            .into_iter()
            .filter(|alternate| Some(*alternate) != family)
        {
            let projection = project_service_vpc_registration(
                service_id,
                carbide_dpf::NAMESPACE,
                &data,
                alternate_family,
                mac,
                effective_service_vpc_slots,
            )
            .map_err(|error| LoadDesiredRegistrationError::InvalidData(error.to_string()))?;
            replacement_service_interfaces.extend(projection.interfaces.into_iter().take(1));
            replacement_services.push(projection.service);
        }
    }

    Ok(DesiredRegistration {
        resources,
        replacement_services,
        replacement_service_interfaces,
        family,
        had_interface_mac: had_interface_mac || network_identity.is_some(),
        has_references,
        capacity_draining,
    })
}

/// Returns whether an expected deterministic name belongs to another extension service.
/// Only the owner label establishes adoption authority; matching specs are insufficient.
fn foreign_expected_resource<'a>(
    service_id: ExtensionServiceId,
    observed: &[DetachedDpfResourceObservation],
    expected_names: impl Iterator<Item = &'a str>,
) -> bool {
    expected_names.into_iter().any(|expected_name| {
        observed.iter().any(|resource| {
            resource.name.as_deref() == Some(expected_name)
                && resource.labels.get(DPF_HELM_CHART_OWNER_LABEL) != Some(&service_id.to_string())
        })
    })
}

/// Reports whether the owned observations differ from the complete desired-name inventory.
/// A terminating desired object is not healthy because its name cannot be safely recreated yet.
fn resource_inventory_requires_repair<'a>(
    service_id: ExtensionServiceId,
    observed: &[DetachedDpfResourceObservation],
    desired_names: impl Iterator<Item = &'a str>,
) -> bool {
    let desired_names = desired_names.collect::<Vec<_>>();
    let owner = service_id.to_string();
    desired_names.iter().any(|desired_name| {
        observed
            .iter()
            .find(|resource| resource.name.as_deref() == Some(*desired_name))
            .is_none_or(|resource| resource.is_deleting)
    }) || observed.iter().any(|resource| {
        resource.labels.get(DPF_HELM_CHART_OWNER_LABEL) == Some(&owner)
            && resource
                .name
                .as_deref()
                .is_none_or(|name| !desired_names.contains(&name))
    })
}

/// Returns whether an Updating registration may replace an intentionally changed network shape.
/// Other immutable drift remains a conflict so NICo never repairs an externally altered identity.
fn dpu_service_replacement_allowed(
    mode: RegistrationReconcileMode,
    has_references: bool,
    conflict: DpuServiceOwnershipConflict,
) -> bool {
    mode == RegistrationReconcileMode::Updating
        && !has_references
        && matches!(
            conflict,
            DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.serviceID",
            } | DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.interfaces",
            } | DpuServiceOwnershipConflict::ImmutableField {
                field: "spec.serviceDaemonSet.annotations[k8s.v1.cni.cncf.io/networks]",
            }
        )
}

/// Deletes obsolete owned chains sequentially or repairs retained chain specifications before
/// dependent interfaces can change.
/// Leaves dependent interfaces unchanged until a later observation confirms every deleted chain is
/// absent.
async fn reconcile_obsolete_chains(
    service_id: ExtensionServiceId,
    dpf_sdk: &dyn DpfOperations,
    observed: &[DetachedDpfResourceObservation],
    desired: &[DetachedDpuServiceChainDefinition],
) -> Option<StateHandlerOutcome<ExtensionServiceLifecycleState>> {
    let mut deletions = Vec::new();
    let mut repairs = Vec::new();
    for existing in observed.iter().filter(|resource| {
        resource.labels.get(DPF_HELM_CHART_OWNER_LABEL) == Some(&service_id.to_string())
    }) {
        if existing.namespace.as_deref() != Some(carbide_dpf::NAMESPACE) {
            return Some(permanent_failure(
                service_id,
                "DPUServiceChain ownership label or immutable fields do not match this registration",
                DpuServiceOwnershipConflict::ImmutableField {
                    field: "metadata.namespace",
                },
            ));
        }
        if existing.is_deleting {
            return Some(StateHandlerOutcome::wait(
                "service-VPC chain finalization is in progress; retrying".to_string(),
            ));
        }
        let expected = existing
            .name
            .as_deref()
            .and_then(|name| desired.iter().find(|definition| definition.name == name));
        if let Some(definition) = expected {
            if let Err(error) = verify_service_chain_identity(existing, service_id, definition) {
                return Some(permanent_failure(
                    service_id,
                    "existing DPUServiceChain ownership label or immutable fields do not match this registration",
                    error,
                ));
            }
            match service_chain_spec_matches(existing, definition) {
                Ok(true) => continue,
                Ok(false) => repairs.push((existing, definition)),
                Err(error) => {
                    return Some(permanent_failure(
                        service_id,
                        "could not project desired DPUServiceChain specification",
                        error,
                    ));
                }
            }
            continue;
        }
        let preconditions = match existing.delete_preconditions() {
            Ok(preconditions) => preconditions,
            Err(error) => {
                return Some(permanent_failure(
                    service_id,
                    "DPUServiceChain deletion requires metadata.uid and metadata.resourceVersion",
                    error,
                ));
            }
        };
        let Some(name) = existing.name.as_deref() else {
            return Some(permanent_failure(
                service_id,
                "DPUServiceChain is missing metadata.name",
                "observed owned chain has no name",
            ));
        };
        deletions.push((name, preconditions));
    }

    // Delete each independent chain sequentially, then return Wait so Kubernetes can confirm
    // finalizer completion before interface removal.
    if !deletions.is_empty() {
        for (name, preconditions) in deletions {
            if let Err(error) = dpf_sdk
                .delete_detached_dpu_service_chain(name, &preconditions)
                .await
            {
                return Some(dpf_failure_outcome(
                    service_id,
                    "service-VPC chain deletion failed; retrying",
                    error,
                ));
            }
        }
        return Some(StateHandlerOutcome::wait(
            "obsolete service-VPC chain deletion is awaiting finalization".to_string(),
        ));
    }

    // The observed resource version fences each repair without another Kubernetes read.
    for (existing, definition) in repairs {
        let Some(name) = existing.name.as_deref() else {
            return Some(permanent_failure(
                service_id,
                "DPUServiceChain is missing metadata.name",
                "observed owned chain has no name",
            ));
        };
        let Some(resource_version) = existing.resource_version.as_deref() else {
            return Some(permanent_failure(
                service_id,
                "DPUServiceChain update requires metadata.resourceVersion",
                "observed DPUServiceChain is missing metadata.resourceVersion",
            ));
        };
        let spec = match detached_dpu_service_chain_spec(definition) {
            Ok(spec) => spec,
            Err(error) => {
                return Some(permanent_failure(
                    service_id,
                    "could not project desired DPUServiceChain specification",
                    error,
                ));
            }
        };
        let patched = match dpf_sdk
            .patch_detached_dpu_service_chain(
                name,
                detached_resource_spec_patch(spec, resource_version),
            )
            .await
        {
            Ok(patched) => patched,
            Err(error) => {
                return Some(dpf_failure_outcome(
                    service_id,
                    "service-VPC chain update failed; retrying",
                    error,
                ));
            }
        };
        if let Err(error) = verify_service_chain_identity(&patched, service_id, definition) {
            return Some(permanent_failure(
                service_id,
                "updated DPUServiceChain ownership label or immutable fields do not match this registration",
                error,
            ));
        }
        if patched.is_deleting {
            return Some(StateHandlerOutcome::wait(
                "service-VPC chain update response has not converged; retrying".to_string(),
            ));
        }
        match service_chain_spec_matches(&patched, definition) {
            Ok(true) => {}
            Ok(false) => {
                return Some(StateHandlerOutcome::wait(
                    "service-VPC chain update response has not converged; retrying".to_string(),
                ));
            }
            Err(error) => {
                return Some(permanent_failure(
                    service_id,
                    "could not validate updated DPUServiceChain specification",
                    error,
                ));
            }
        }
    }
    None
}

/// Deletes obsolete interfaces sequentially after chains are absent or repairs retained interface
/// specifications.
/// Waits for finalizer-confirmed absence before reusing a deterministic name.
async fn reconcile_obsolete_interfaces(
    service_id: ExtensionServiceId,
    dpf_sdk: &dyn DpfOperations,
    observed: &[DetachedDpfResourceObservation],
    desired: &[DetachedDpuServiceInterfaceDefinition],
    replace_service_interface_drift: bool,
) -> Option<StateHandlerOutcome<ExtensionServiceLifecycleState>> {
    let mut deletions = Vec::new();
    let mut repairs = Vec::new();
    for existing in observed.iter().filter(|resource| {
        resource.labels.get(DPF_HELM_CHART_OWNER_LABEL) == Some(&service_id.to_string())
    }) {
        if existing.namespace.as_deref() != Some(carbide_dpf::NAMESPACE) {
            return Some(permanent_failure(
                service_id,
                "DPUServiceInterface ownership label or immutable fields do not match this registration",
                DpuServiceOwnershipConflict::ImmutableField {
                    field: "metadata.namespace",
                },
            ));
        }
        if existing.is_deleting {
            return Some(StateHandlerOutcome::wait(
                "service-VPC interface finalization is in progress; retrying".to_string(),
            ));
        }
        let expected = existing
            .name
            .as_deref()
            .and_then(|name| desired.iter().find(|definition| definition.name == name));
        if let Some(definition) = expected {
            let replace = replace_service_interface_drift
                && desired.first().map(|first| first.name.as_str())
                    == Some(definition.name.as_str());
            if !replace {
                if let Err(error) =
                    verify_service_interface_identity(existing, service_id, definition)
                {
                    return Some(permanent_failure(
                        service_id,
                        "existing DPUServiceInterface ownership label or immutable fields do not match this registration",
                        error,
                    ));
                }
                match service_interface_spec_matches(existing, definition) {
                    Ok(true) => continue,
                    Ok(false) => repairs.push((existing, definition)),
                    Err(error) => {
                        return Some(permanent_failure(
                            service_id,
                            "could not project desired DPUServiceInterface specification",
                            error,
                        ));
                    }
                }
                continue;
            }
        }
        let preconditions = match existing.delete_preconditions() {
            Ok(preconditions) => preconditions,
            Err(error) => {
                return Some(permanent_failure(
                    service_id,
                    "DPUServiceInterface deletion requires metadata.uid and metadata.resourceVersion",
                    error,
                ));
            }
        };
        let Some(name) = existing.name.as_deref() else {
            return Some(permanent_failure(
                service_id,
                "DPUServiceInterface is missing metadata.name",
                "observed owned interface has no name",
            ));
        };
        deletions.push((name, preconditions));
    }

    // Delete each interface sequentially after every chain is absent, then return Wait for
    // finalizer confirmation.
    if !deletions.is_empty() {
        for (name, preconditions) in deletions {
            if let Err(error) = dpf_sdk
                .delete_detached_dpu_service_interface(name, &preconditions)
                .await
            {
                return Some(dpf_failure_outcome(
                    service_id,
                    "service-VPC interface deletion failed; retrying",
                    error,
                ));
            }
        }
        return Some(StateHandlerOutcome::wait(
            "obsolete service-VPC interface deletion is awaiting finalization".to_string(),
        ));
    }

    // The observed resource version fences each repair without another Kubernetes read.
    for (existing, definition) in repairs {
        let Some(name) = existing.name.as_deref() else {
            return Some(permanent_failure(
                service_id,
                "DPUServiceInterface is missing metadata.name",
                "observed owned interface has no name",
            ));
        };
        let Some(resource_version) = existing.resource_version.as_deref() else {
            return Some(permanent_failure(
                service_id,
                "DPUServiceInterface update requires metadata.resourceVersion",
                "observed DPUServiceInterface is missing metadata.resourceVersion",
            ));
        };
        let spec = match detached_dpu_service_interface_spec(definition) {
            Ok(spec) => spec,
            Err(error) => {
                return Some(permanent_failure(
                    service_id,
                    "could not project desired DPUServiceInterface specification",
                    error,
                ));
            }
        };
        let patched = match dpf_sdk
            .patch_detached_dpu_service_interface(
                name,
                detached_resource_spec_patch(spec, resource_version),
            )
            .await
        {
            Ok(patched) => patched,
            Err(error) => {
                return Some(dpf_failure_outcome(
                    service_id,
                    "service-VPC interface update failed; retrying",
                    error,
                ));
            }
        };
        if let Err(error) = verify_service_interface_identity(&patched, service_id, definition) {
            return Some(permanent_failure(
                service_id,
                "updated DPUServiceInterface ownership label or immutable fields do not match this registration",
                error,
            ));
        }
        if patched.is_deleting {
            return Some(StateHandlerOutcome::wait(
                "service-VPC interface update response has not converged; retrying".to_string(),
            ));
        }
        match service_interface_spec_matches(&patched, definition) {
            Ok(true) => {}
            Ok(false) => {
                return Some(StateHandlerOutcome::wait(
                    "service-VPC interface update response has not converged; retrying".to_string(),
                ));
            }
            Err(error) => {
                return Some(permanent_failure(
                    service_id,
                    "could not validate updated DPUServiceInterface specification",
                    error,
                ));
            }
        }
    }
    None
}

/// Removes owned DPF parents in dependency order, then releases the MAC under the service lock.
/// The lifecycle reaches Deleted only after finalizer-confirmed absence and a repeated reference
/// check.
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

    // Require DPUService absence before removing its chain and interface parents because
    // Kubernetes accepts deletion before DPF finalizers complete.
    match dpf_sdk.get_dpu_service(&expected_name).await {
        Ok(Some(existing)) => {
            if let Err(error) = verify_dpu_service_owner_label(&existing, service_id) {
                return Ok(permanent_failure(
                    service_id,
                    "existing DPUService owner label or immutable fields do not match this registration",
                    error,
                ));
            }
            if existing.namespace.as_deref() != Some(carbide_dpf::NAMESPACE) {
                return Ok(permanent_failure(
                    service_id,
                    "existing DPUService owner label or immutable fields do not match this registration",
                    DpuServiceOwnershipConflict::ImmutableField {
                        field: "metadata.namespace",
                    },
                ));
            }
            if existing.is_deleting {
                return Ok(StateHandlerOutcome::wait(
                    "DPF DPUService finalization is in progress; retrying".to_string(),
                ));
            }
            let preconditions = match existing.delete_preconditions() {
                Ok(preconditions) => preconditions,
                Err(error) => {
                    return Ok(permanent_failure(
                        service_id,
                        "DPUService deletion requires metadata.uid and metadata.resourceVersion",
                        error,
                    ));
                }
            };
            return Ok(
                match dpf_sdk
                    .delete_dpu_service(&expected_name, &preconditions)
                    .await
                {
                    Ok(()) => StateHandlerOutcome::wait(
                        "DPF accepted DPUService deletion; awaiting finalization".to_string(),
                    ),
                    Err(error) => dpf_failure_outcome(
                        service_id,
                        "DPUService deletion failed; retrying",
                        error,
                    ),
                },
            );
        }
        Ok(None) => {}
        Err(error) if is_not_found(&error) => {}
        Err(error) => {
            return Ok(dpf_failure_outcome(
                service_id,
                "could not read DPUService before deletion; retrying",
                error,
            ));
        }
    }

    // Skip Kubernetes cleanup when no MAC was allocated because service-VPC parent creation
    // cannot precede that durable identity.
    let has_interface_mac = {
        let mut connection = ctx.services.db_pool.acquire().await?;
        !db::extension_service::find_interface_macs(&mut connection, service_id)
            .await?
            .is_empty()
    };
    if !has_interface_mac {
        return Ok(StateHandlerOutcome::transition(
            ExtensionServiceLifecycleState::Deleted,
        ));
    }

    // Retire chains before their interface endpoints, but only after the consuming DPUService is
    // confirmed absent.
    let owner_selector = format!("{DPF_HELM_CHART_OWNER_LABEL}={service_id}");
    let chains = match dpf_sdk
        .list_detached_dpu_service_chains(Some(&owner_selector))
        .await
    {
        Ok(chains) => chains,
        Err(error) => {
            return Ok(dpf_failure_outcome(
                service_id,
                "could not list service-VPC chains before deletion; retrying",
                error,
            ));
        }
    };
    if let Some(outcome) =
        reconcile_obsolete_chains(service_id, dpf_sdk.as_ref(), &chains, &[]).await
    {
        return Ok(outcome);
    }

    let interfaces = match dpf_sdk
        .list_detached_dpu_service_interfaces(Some(&owner_selector))
        .await
    {
        Ok(interfaces) => interfaces,
        Err(error) => {
            return Ok(dpf_failure_outcome(
                service_id,
                "could not list service-VPC interfaces before deletion; retrying",
                error,
            ));
        }
    };
    if let Some(outcome) =
        reconcile_obsolete_interfaces(service_id, dpf_sdk.as_ref(), &interfaces, &[], false).await
    {
        return Ok(outcome);
    }

    // Serialize the final reference check and MAC release with attachment writers.
    let mut txn = db::Transaction::begin(&ctx.services.db_pool).await?;
    let service_exists =
        !db::extension_service::find_by_ids(txn.as_mut(), &[service_id], true, true)
            .await?
            .is_empty();
    if !service_exists {
        return Ok(permanent_failure(
            service_id,
            "extension-service row disappeared before MAC release",
            "extension service not found",
        ));
    }
    if db::extension_service::is_service_in_use(txn.as_mut(), service_id, &[], true).await? {
        return Ok(StateHandlerOutcome::wait(
            "extension service still has active or terminating references; retrying deletion"
                .to_string(),
        ));
    }
    // Release the MAC last so reference checks and parent cleanup complete before address reuse.
    db::extension_service::delete_interface_macs(txn.as_mut(), service_id).await?;
    txn.commit().await?;

    // Return Deleted only after MAC release commits; the state controller persists the lifecycle
    // transition separately.
    Ok(StateHandlerOutcome::transition(
        ExtensionServiceLifecycleState::Deleted,
    ))
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

/// Converts a DPF failure into a permanent lifecycle failure or an explicit retry outcome.
/// Only request-shape errors are terminal because transport and conflict failures can converge
/// later.
fn dpf_failure_outcome(
    service_id: ExtensionServiceId,
    operation: &'static str,
    error: DpfError,
) -> StateHandlerOutcome<ExtensionServiceLifecycleState> {
    if is_permanent_dpf_error(&error) {
        return permanent_failure(service_id, operation, error);
    }
    tracing::warn!(%service_id, %error, operation, "DPF Helm chart reconciliation will retry");
    StateHandlerOutcome::wait(operation.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies a reduced configured count retains every slot through the highest persisted
    /// `slot_index` because Ready would otherwise authorize deleting its DPUServiceChain.
    #[test]
    fn effective_slots_preserve_retained_parent_during_capacity_drain() {
        // Model one configured slot and a highest persisted service-interface `slot_index` of three.
        let configured = carbide_dpf::ServiceVpcSlots::new(1).expect("valid configured slots");

        // Derive the `ServiceVpcSlots` used by registration reconciliation.
        let (effective, draining) =
            effective_service_vpc_slots(configured, Some(3)).expect("retained slot is supported");

        // The effective inventory contains slots zero through three and reports that Ready remains blocked.
        assert_eq!(effective.len(), 4);
        assert!(draining);
    }

    /// Verifies a converged Updating registration stays non-Ready while persisted service interfaces
    /// require slots above configured capacity, preventing deletion of their DPUServiceInterface and
    /// DPUServiceChain resources.
    #[test]
    fn converged_updating_registration_waits_during_capacity_drain() {
        // Pass `Updating` with `ready_allowed` false after DPUService fields have converged.
        let outcome = converged_dpu_service_outcome(RegistrationReconcileMode::Updating, false);

        // Reconciliation remains waiting until service-interface reclamation permits the slot shrink.
        assert!(matches!(
            outcome,
            StateHandlerOutcome::Wait { reason, .. }
                if reason == "service-VPC slot capacity is draining retained endpoints; retrying"
        ));
    }

    /// Verifies Kubernetes 409 responses remain retryable because a refreshed observation can
    /// make the same operation safe.
    #[test]
    fn kubernetes_conflict_produces_a_wait_outcome() {
        // Construct the status returned by Kubernetes when a conditional write loses a race.
        let conflict = DpfError::KubeError(kube::Error::Api(
            kube::core::Status {
                code: 409,
                ..Default::default()
            }
            .boxed(),
        ));

        // Require a retryable Wait instead of a permanent Failed transition.
        assert!(matches!(
            dpf_failure_outcome(
                ExtensionServiceId::new(),
                "DPUService deletion failed; retrying",
                conflict,
            ),
            StateHandlerOutcome::Wait {
                reason,
                requeue_immediately: false,
                ..
            } if reason == "DPUService deletion failed; retrying"
        ));
    }
}
