// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Reconcile persisted service endpoints without releasing allocations before observed isolation.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;

use carbide_dpf::{
    DetachedDpuServiceChainDefinition, DpfError, DpfResourcePreconditions,
    DpuDevicePlacementObservation, ServiceVpcChainIntent, ServiceVpcChainRequest,
    ServiceVpcChainRequestContext, ServiceVpcChainState,
};
use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::machine::DpuMachineId;
use chrono::Utc;
use db::extension_service::ServiceVpcChainExclusion;
use db::{ConditionalWrite, TransactionVending as _};
use futures::{StreamExt as _, stream};
use model::extension_service::service_vpc::{
    ServiceVpcEndpointStatusObservation, network_acknowledged,
};
use model::extension_service::{
    DPF_HELM_CHART_PLACEMENT_LABEL_VALUE, DpfHelmChartIdentity, ExtensionServiceChainSelector,
    ExtensionServiceType,
};
use model::instance::snapshot::InstanceSnapshot;
use model::instance::status::extension_service::InstanceExtensionServiceStatusObservation;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{DpuMachine, HostMachine, ManagedHostState, ManagedHostStateSnapshot};
use model::resource_pool::common::CommonPools;
use state_controller::CheckApplied as _;
use state_controller::state_handler::StateHandlerError;
use tokio::time::Instant;

use super::{PlacementEvidence, get_extension_service_types_for_instance};
use crate::dpf::DpfOperations;

/// Reconciles fixed endpoint DPUs and retains current evidence through restart; returns true after
/// a configuration change or stale pass so the caller reloads before any later lifecycle action.
///
/// External phases share half the actual remaining handler budget, leaving time for final database
/// operations and ordinary machine handling. Remaining phases divide unused external time equally;
/// these allocations are scheduling policy, not cleanup evidence. Database commits remain subject
/// to the enclosing controller deadline rather than local I/O cancellation.
pub(in crate::handler) async fn reconcile(
    host: &mut ManagedHostStateSnapshot,
    pool: &sqlx::PgPool,
    sdk: Option<&dyn DpfOperations>,
    common_pools: Option<&CommonPools>,
    deadline: Instant,
) -> Result<bool, StateHandlerError> {
    let Some(instance) = host
        .instance
        .as_ref()
        .filter(|instance| !instance.config.network.service_interfaces.is_empty())
        .cloned()
    else {
        return Ok(false);
    };

    // Persist removal generations before reset or deletion can withdraw network configuration.
    if instance.deleted.is_some() || matches!(host.managed_state, ManagedHostState::Reset { .. }) {
        let mut txn = pool
            .txn_begin()
            .await
            .map_err(StateHandlerError::from_transaction)?;
        // Fence the saved reset or release request before a reused UUID can retire a replacement.
        let current = db::instance::find_by_id_for_update(txn.as_mut(), instance.id).await?;
        if !current
            .as_ref()
            .is_some_and(|current| matches_instance(current, &instance))
        {
            txn.commit()
                .await
                .map_err(StateHandlerError::from_transaction)?;
            return Ok(true);
        }
        let changed =
            db::instance::terminate_service_vpc_attachments(txn.as_mut(), instance.id, Utc::now())
                .await?;
        txn.commit()
            .await
            .map_err(StateHandlerError::from_transaction)?;
        if changed {
            return Ok(true);
        }
    }

    let sdk = sdk.filter(|_| host.host_snapshot.config.dpf.used_for_ingestion);
    let started_at = Utc::now();
    let service_types = get_extension_service_types_for_instance(&instance, pool).await?;
    let configurations = instance
        .config
        .extension_services
        .service_configs
        .iter()
        .filter(|config| {
            service_types.get(&config.service_id) == Some(&ExtensionServiceType::DpfHelmChart)
        })
        .collect::<Vec<_>>();
    let endpoint_dpus = instance
        .config
        .network
        .service_interfaces
        .iter()
        .map(|endpoint| endpoint.dpu_id)
        .collect::<BTreeSet<_>>();
    let mut placements = BTreeMap::<DpuMachineId, DpuDevicePlacementObservation>::new();
    let mut errors = HashMap::<DpuMachineId, String>::new();
    let mut missing_devices = BTreeSet::new();

    // Reserve half the remaining object budget for final database writes and ordinary machine work.
    let phase_started_at = Instant::now();
    let external_deadline =
        phase_started_at + deadline.saturating_duration_since(phase_started_at) / 2;
    let placement_deadline =
        phase_started_at + external_deadline.saturating_duration_since(phase_started_at) / 4;

    // Capture device revisions before checking persisted desire so a later withdrawal fences
    // every older placement write. Bound independent reads to four DPUs and divide their phase
    // among finite waves so slow DPUs leave time for later placement and lifecycle work.
    let placement_reads = host
        .dpu_snapshots
        .iter()
        .map(|dpu| (dpu.id, read_dpu_placement(sdk, dpu, &endpoint_dpus)))
        .collect::<Vec<_>>();
    let placement_reads = collect_placement_attempts(placement_reads, placement_deadline).await;
    for (dpu_id, completed) in placement_reads {
        let result = completed.unwrap_or_else(|| {
            Err(DpfError::timeout(
                "read service-VPC placement",
                "controller placement phase budget exhausted",
            ))
        });
        match result {
            Ok(observed) => {
                placements.insert(dpu_id, observed);
            }
            Err(error) => {
                tracing::warn!(dpu_machine_id = %dpu_id, %error, "service VPC placement read failed");
                if error.is_not_found() {
                    missing_devices.insert(dpu_id);
                }
                errors.insert(dpu_id, error.to_string());
            }
        }
    }

    // Resolve only previously unrecorded Node identities. A retained Node is never recaptured
    // during retirement, including after discovery changes or a process restart.
    let unresolved = host
        .dpu_snapshots
        .iter()
        .filter(|dpu| endpoint_dpus.contains(&dpu.id) && placements.contains_key(&dpu.id))
        .filter(|dpu| {
            instance
                .config
                .network
                .service_interfaces
                .iter()
                .filter(|endpoint| endpoint.dpu_id == dpu.id)
                .any(|endpoint| {
                    !dpu.status
                        .extension_service_status_observations
                        .for_service_type(ExtensionServiceType::DpfHelmChart)
                        .is_some_and(|observation| {
                            observation.service_vpc_endpoints.iter().any(|previous| {
                                previous.endpoint == *endpoint && previous.node.is_some()
                            })
                        })
                })
        })
        .filter_map(|dpu| dpu.dpf_id().map(|device| (dpu.id, device)))
        .collect::<BTreeMap<_, _>>();
    let mut resolved_nodes = BTreeMap::new();
    if !unresolved.is_empty()
        && let Some(sdk) = sdk
    {
        // Donate unused read time while retaining equal shares for placement and chain observation.
        let phase_started_at = Instant::now();
        let node_deadline =
            phase_started_at + external_deadline.saturating_duration_since(phase_started_at) / 3;
        match sdk
            .resolve_service_vpc_nodes(&unresolved, node_deadline)
            .await
        {
            Ok(nodes) => {
                for dpu_id in unresolved
                    .keys()
                    .filter(|dpu_id| !nodes.contains_key(*dpu_id))
                {
                    errors.entry(*dpu_id).or_insert_with(|| {
                        "Waiting for a current unique remote Node identity on this DPU.".to_owned()
                    });
                }
                resolved_nodes = nodes;
            }
            Err(error) => {
                tracing::warn!(%error, "service VPC Node identity resolution failed");
                for dpu_id in unresolved.keys() {
                    errors.entry(*dpu_id).or_insert_with(|| error.to_string());
                }
            }
        }
    }

    let mut observations = BTreeMap::new();
    let mut txn = pool
        .txn_begin()
        .await
        .map_err(StateHandlerError::from_transaction)?;
    let Some(locked) = lock_reconciliation_state(txn.as_mut(), &instance, false).await? else {
        txn.commit()
            .await
            .map_err(StateHandlerError::from_transaction)?;
        return Ok(true);
    };
    if !matches_instance(&locked.instance, &instance)
        || locked.host.network_config.version != host.host_snapshot.network_config.version
        || locked.host.state.value != host.managed_state
    {
        txn.commit()
            .await
            .map_err(StateHandlerError::from_transaction)?;
        return Ok(true);
    }
    let mut selectors = locked.selectors;

    // Reload stored Node identities after locking the Instance, host, complete DPU group and
    // selectors so neither network receipts nor shared exclusions can change during publication.
    for (dpu_id, dpu) in &locked.dpus {
        let previous = dpu
            .status
            .extension_service_status_observations
            .for_service_type(ExtensionServiceType::DpfHelmChart);
        let mut endpoints = Vec::new();
        for endpoint in instance
            .config
            .network
            .service_interfaces
            .iter()
            .filter(|endpoint| endpoint.dpu_id == *dpu_id)
        {
            let config = configurations
                .iter()
                .find(|config| config.id == Some(endpoint.attachment_id))
                .ok_or_else(|| {
                    StateHandlerError::GenericError(eyre::eyre!(
                        "service endpoint has no DPF attachment"
                    ))
                })?;
            let previous = previous.and_then(|observation| {
                observation
                    .service_vpc_endpoints
                    .iter()
                    .find(|previous| previous.endpoint == *endpoint)
            });
            // No previous Node means no placement was authorized by this controller. Capturing
            // the initial Node also permits cancellation before the first active reconciliation.
            let node = previous
                .and_then(|previous| previous.node.clone())
                .or_else(|| resolved_nodes.get(dpu_id).cloned());
            let intent = if config.removed.is_some() {
                ServiceVpcChainIntent::Retiring
            } else {
                ServiceVpcChainIntent::Active
            };
            endpoints.push(ServiceVpcEndpointStatusObservation {
                endpoint: endpoint.clone(),
                context: ServiceVpcChainRequestContext {
                    attachment_id: endpoint.attachment_id,
                    internal_uuid: endpoint.internal_uuid,
                    service_id: config.service_id,
                    interface_ordinal: endpoint.interface_ordinal,
                    slot_index: endpoint.slot_index,
                    dpu_id: *dpu_id,
                    extension_services_config_version: instance.extension_services_config_version,
                    network_config_version: instance.network_config_version,
                    selector_version: config_version::ConfigVersion::initial(),
                    intent,
                },
                managed_host_config_version: host.host_snapshot.network_config.version,
                node,
                cleanup_bypass_observed: previous
                    .is_some_and(|previous| previous.cleanup_bypass_observed),
                placement: None,
                placement_observed: false,
                network_acknowledged: false,
                chain_converged: false,
                chain_isolated: false,
                registration: None,
                set: None,
                chains: Vec::new(),
                chain_list_resource_version: None,
                message: errors.get(dpu_id).cloned().unwrap_or_else(|| {
                    "Waiting for current chain and network evidence.".to_owned()
                }),
            });
        }
        observations.insert(
            *dpu_id,
            InstanceExtensionServiceStatusObservation {
                config_version: if sdk.is_some() {
                    instance.extension_services_config_version
                } else {
                    previous
                        .map(|observation| observation.config_version)
                        .unwrap_or(instance.extension_services_config_version)
                },
                instance_config_version: Some(instance.config_version),
                extension_service_statuses: previous
                    .map(|observation| observation.extension_service_statuses.clone())
                    .unwrap_or_default(),
                observed_at: started_at,
                service_vpc_endpoints: endpoints,
            },
        );
    }

    // Commit every DPU's selector desire before any external placement mutation, then use the
    // final shared selector versions for every request in this pass.
    let mut endpoint_changes = observations
        .values()
        .flat_map(|observation| &observation.service_vpc_endpoints)
        .collect::<Vec<_>>();
    endpoint_changes.sort_by_key(|evidence| {
        (
            evidence.context.service_id,
            evidence.context.interface_ordinal,
            evidence.context.slot_index,
            evidence.context.dpu_id,
        )
    });
    for evidence in endpoint_changes {
        let context = &evidence.context;
        let key = (
            context.service_id,
            context.interface_ordinal,
            context.slot_index,
        );
        let change = if context.intent == ServiceVpcChainIntent::Active && evidence.node.is_some() {
            ServiceVpcChainExclusion::Allow
        } else {
            ServiceVpcChainExclusion::Exclude
        };
        // The selector row is already locked, so unchanged membership needs no UPDATE.
        if selectors.get(&key).is_some_and(|selector| {
            selector.excluded_dpu_ids.contains(&context.dpu_id)
                == matches!(change, ServiceVpcChainExclusion::Exclude)
        }) {
            continue;
        }
        let updated = db::extension_service::try_update_chain_exclusion(
            txn.as_mut(),
            context.service_id,
            context.interface_ordinal,
            context.slot_index,
            context.dpu_id,
            selectors.get(&key).map(|selector| selector.version),
            change,
        )
        .await?;
        let ConditionalWrite::Applied(selector) = updated else {
            return Err(StateHandlerError::GenericError(eyre::eyre!(
                "service chain selector changed; retry reconciliation"
            )));
        };
        selectors.insert(key, selector);
    }
    for (dpu_id, observation) in &mut observations {
        for evidence in &mut observation.service_vpc_endpoints {
            let context = &mut evidence.context;
            context.selector_version = selectors
                .get(&(
                    context.service_id,
                    context.interface_ordinal,
                    context.slot_index,
                ))
                .ok_or_else(|| {
                    StateHandlerError::GenericError(eyre::eyre!("missing committed chain selector"))
                })?
                .version;
        }
        // Preserve current placement and chain evidence while an unchanged refresh is in flight,
        // but retain a prior result only while its complete endpoint set and every desired context still match.
        let mut persisted = observation.clone();
        if !errors.contains_key(dpu_id)
            && let Some(dpu) = locked.dpus.get(dpu_id)
            && let Some(previous) = dpu
                .status
                .extension_service_status_observations
                .for_service_type(ExtensionServiceType::DpfHelmChart)
                .filter(|previous| {
                    previous.config_version == observation.config_version
                        && previous.instance_config_version == observation.instance_config_version
                        && previous.service_vpc_endpoints.len()
                            == observation.service_vpc_endpoints.len()
                        && previous.service_vpc_endpoints.iter().all(|previous| {
                            observation
                                .service_vpc_endpoints
                                .iter()
                                .any(|evidence| previous.endpoint == evidence.endpoint)
                        })
                })
        {
            for evidence in &mut persisted.service_vpc_endpoints {
                if let Some(previous) = previous.service_vpc_endpoints.iter().find(|previous| {
                    previous.endpoint == evidence.endpoint
                        && previous.context == evidence.context
                        && previous.node == evidence.node
                        && selectors
                            .get(&(
                                evidence.context.service_id,
                                evidence.context.interface_ordinal,
                                evidence.context.slot_index,
                            ))
                            .is_some_and(|selector| {
                                previous.matches_desired_context(
                                    &locked.host,
                                    &instance,
                                    dpu,
                                    selector,
                                )
                            })
                }) {
                    evidence.clone_from(previous);
                    // Recheck network acknowledgement so retained DPF facts cannot reuse an older receipt.
                    evidence.network_acknowledged = network_acknowledged(
                        &locked.host,
                        &locked.host.state.value,
                        &instance,
                        dpu,
                        evidence.context.intent,
                    );
                    if !evidence.network_acknowledged {
                        evidence.message = "Waiting for applied HBN configuration and successful DHCP submission for the current network generation.".to_owned();
                    }
                }
            }
        }
        db::machine::update_extension_service_status_observation(
            txn.as_mut(),
            dpu_id,
            ExtensionServiceType::DpfHelmChart,
            &persisted,
        )
        .await?
        .check_applied()?;
    }
    txn.commit()
        .await
        .map_err(StateHandlerError::from_transaction)?;

    // Revalidate after the device read and immediately before each fenced write; readback must
    // carry this exact Instance and configuration stamp, even when withdrawal changed no labels.
    let placement_config = format!(
        "{}:{}:{}",
        instance.id, instance.extension_services_config_version, instance.network_config_version
    );
    let attached = host
        .dpu_snapshots
        .iter()
        .map(|dpu| dpu.id)
        .collect::<Vec<_>>();
    let used = host
        .host_snapshot
        .get_dpu_device_and_id_mappings()
        .ok()
        .map(|(_, mapping)| {
            instance.config.network.get_used_dpus(
                &mapping,
                host.host_snapshot.primary_attached_dpu_machine_id(),
            )
        });
    let mut readbacks = BTreeMap::new();
    if let Some(sdk) = sdk {
        // Leave the last external share for chain evidence even when placement cannot finish.
        let phase_started_at = Instant::now();
        let placement_deadline =
            phase_started_at + external_deadline.saturating_duration_since(phase_started_at) / 2;
        let placement_writes = host
            .dpu_snapshots
            .iter()
            .filter_map(|dpu| Some((dpu, placements.get(&dpu.id)?, dpu.dpf_id()?)))
            .map(|(dpu, before, device)| {
                let instance = &instance;
                let observations = &observations;
                let configurations = &configurations;
                let placement_config = &placement_config;
                let attached = &attached;
                let used = &used;
                let endpoint_dpus = &endpoint_dpus;
                let primary = host.host_snapshot.primary_attached_dpu_machine_id();
                (dpu.id, async move {
                    let current = db::instance::find_by_id(pool, instance.id).await?;
                    if !current.as_ref().is_some_and(|current| {
                        current.config.extension_services == instance.config.extension_services
                            && current.config.network == instance.config.network
                            && current.extension_services_config_version
                                == instance.extension_services_config_version
                            && current.network_config_version == instance.network_config_version
                    }) {
                        return Ok(None);
                    }
                    let changes = placement_changes(
                        instance,
                        configurations,
                        dpu.id,
                        before,
                        observations.get(&dpu.id),
                        attached,
                        used.as_deref(),
                        primary,
                    );
                    let result = async {
                        sdk.patch_dpu_device_placement(
                            &device,
                            changes.clone(),
                            placement_config,
                            &DpfResourcePreconditions {
                                uid: before.resource.uid.clone(),
                                resource_version: before.resource.resource_version.clone(),
                            },
                        )
                        .await?;
                        let readback = sdk
                            .get_dpu_device_placement(
                                &device,
                                endpoint_dpus.contains(&dpu.id).then_some(dpu.id),
                            )
                            .await?;
                        if readback.resource.uid != before.resource.uid
                            || readback.config_version.as_deref() != Some(placement_config.as_str())
                            || !changes.iter().all(|(key, desired)| {
                                readback.node_labels.get(key) == desired.as_ref()
                            })
                        {
                            return Err(carbide_dpf::DpfError::InvalidState(
                            "placement readback does not match current withdrawal or activation"
                                .to_owned(),
                        ));
                        }
                        Ok(readback)
                    }
                    .await;
                    Ok::<_, StateHandlerError>(Some(result))
                })
            })
            .collect::<Vec<_>>();
        let placement_writes =
            collect_placement_attempts(placement_writes, placement_deadline).await;
        for (dpu_id, completed) in placement_writes {
            // Keep local placement expiry incomplete while preserving actual database and stale-pass results.
            let result = match completed {
                Some(result) => result?,
                None => Some(Err(DpfError::timeout(
                    "update service-VPC placement",
                    "controller placement phase budget exhausted",
                ))),
            };
            let Some(result) = result else {
                return Ok(true);
            };
            match result {
                Ok(readback) => {
                    readbacks.insert(dpu_id, readback);
                }
                Err(error) => {
                    tracing::warn!(dpu_machine_id = %dpu_id, %error, "service VPC placement update failed");
                    if error.is_not_found() {
                        missing_devices.insert(dpu_id);
                    }
                    errors.insert(dpu_id, error.to_string());
                }
            }
        }

        // The batched observer validates current registration/set/Node identities and reads
        // chain absence only after the exact exclusion is acknowledged.
        let requests = observations
            .iter()
            .flat_map(|(dpu_id, observation)| {
                let placement = placements.get(dpu_id);
                let selectors = &selectors;
                observation
                    .service_vpc_endpoints
                    .iter()
                    .filter_map(move |evidence| {
                        let node = evidence.node.clone()?;
                        let placement = placement?;
                        let context = &evidence.context;
                        let selector = selectors.get(&(
                            context.service_id,
                            context.interface_ordinal,
                            context.slot_index,
                        ))?;
                        Some(ServiceVpcChainRequest {
                            request_context: context.clone(),
                            definition: DetachedDpuServiceChainDefinition::for_service_vpc(
                                context.service_id,
                                &placement.resource.namespace,
                                context.interface_ordinal,
                                context.slot_index,
                                Some(selector.version),
                                selector.excluded_dpu_ids.clone(),
                            ),
                            node,
                            cleanup_bypass_observed: evidence.cleanup_bypass_observed,
                        })
                    })
            })
            .collect::<Vec<_>>();
        let chain_observations = if requests.is_empty() {
            Vec::new()
        } else {
            sdk.observe_service_vpc_chains(&requests, external_deadline)
                .await
        };
        for observed in chain_observations {
            let Some(evidence) = observations
                .get_mut(&observed.request_context.dpu_id)
                .and_then(|observation| {
                    observation
                        .service_vpc_endpoints
                        .iter_mut()
                        .find(|evidence| evidence.context == observed.request_context)
                })
            else {
                continue;
            };
            evidence.cleanup_bypass_observed |= observed.cleanup_bypass_observed;
            evidence.registration = observed.registration;
            evidence.set = observed.set;
            evidence.chains = observed.chains;
            evidence.chain_list_resource_version = observed.chain_list_resource_version;
            if observed.node == evidence.node && !evidence.cleanup_bypass_observed {
                evidence.chain_converged =
                    matches!(observed.state, ServiceVpcChainState::Converged);
                evidence.chain_isolated = matches!(observed.state, ServiceVpcChainState::Isolated);
            }
            evidence.message = match observed.state {
                ServiceVpcChainState::ReadFailure(error) => error.to_string(),
                _ => observed.reason.unwrap_or_default().to_owned(),
            };
        }
    }

    // Keep finalizer-bypass evidence even if desire changed during external I/O, but discard
    // conclusive stale completions. Reload network acknowledgement before publishing status.
    let mut txn = pool
        .txn_begin()
        .await
        .map_err(StateHandlerError::from_transaction)?;
    let Some(locked) = lock_reconciliation_state(txn.as_mut(), &instance, false).await? else {
        txn.commit()
            .await
            .map_err(StateHandlerError::from_transaction)?;
        return Ok(true);
    };
    let current_pass = matches_instance(&locked.instance, &instance)
        && locked.host.network_config.version == host.host_snapshot.network_config.version
        && locked.host.state.value == host.managed_state;
    for (dpu_id, observation) in &mut observations {
        let Some(dpu) = locked.dpus.get(dpu_id) else {
            continue;
        };
        if !current_pass {
            if let Some(mut retained) = dpu
                .status
                .extension_service_status_observations
                .for_service_type(ExtensionServiceType::DpfHelmChart)
                .cloned()
            {
                for previous in &mut retained.service_vpc_endpoints {
                    previous.cleanup_bypass_observed |=
                        observation.service_vpc_endpoints.iter().any(|evidence| {
                            evidence.endpoint == previous.endpoint
                                && evidence.node == previous.node
                                && evidence.cleanup_bypass_observed
                        });
                }
                retained.observed_at = Utc::now();
                db::machine::update_extension_service_status_observation(
                    txn.as_mut(),
                    dpu_id,
                    ExtensionServiceType::DpfHelmChart,
                    &retained,
                )
                .await?
                .check_applied()?;
            }
            continue;
        }
        let readback = readbacks.get(dpu_id);
        let retained = dpu
            .status
            .extension_service_status_observations
            .for_service_type(ExtensionServiceType::DpfHelmChart);
        for evidence in &mut observation.service_vpc_endpoints {
            evidence.cleanup_bypass_observed |= retained.is_some_and(|retained| {
                retained.service_vpc_endpoints.iter().any(|previous| {
                    previous.endpoint == evidence.endpoint
                        && previous.node == evidence.node
                        && previous.cleanup_bypass_observed
                })
            });
            let identity = DpfHelmChartIdentity::from_service_id(evidence.context.service_id);
            let keys = [
                identity.placement_label_key.clone(),
                identity.slot_placement_label_key(
                    evidence.context.interface_ordinal,
                    evidence.context.slot_index,
                ),
            ];
            evidence.placement = readback.map(|readback| readback.resource.clone());
            evidence.placement_observed = readback.is_some_and(|readback| {
                keys.iter().all(|key| {
                    if evidence.context.intent == ServiceVpcChainIntent::Active {
                        readback
                            .node_labels
                            .get(key)
                            .is_some_and(|value| value == DPF_HELM_CHART_PLACEMENT_LABEL_VALUE)
                    } else {
                        !readback.node_labels.contains_key(key)
                    }
                })
            });
            evidence.network_acknowledged = network_acknowledged(
                &locked.host,
                &locked.host.state.value,
                &instance,
                dpu,
                evidence.context.intent,
            );
            // The host, DPU receipt and selector stay locked through this write, so stale
            // external completions cannot publish Running or Terminated for another context.
            // Retain current placement and chain evidence while network acknowledgement is pending.
            if !locked
                .selectors
                .get(&(
                    evidence.context.service_id,
                    evidence.context.interface_ordinal,
                    evidence.context.slot_index,
                ))
                .is_some_and(|selector| {
                    evidence.matches_desired_context(&locked.host, &instance, dpu, selector)
                })
            {
                evidence.placement_observed = false;
                evidence.chain_converged = false;
                evidence.chain_isolated = false;
                evidence.message =
                    "Waiting for current host, network and chain-selector evidence.".to_owned();
            }
            if let Some(error) = errors.get(dpu_id) {
                evidence.message = error.clone();
            } else if !evidence.network_acknowledged {
                evidence.message = "Waiting for applied HBN configuration and successful DHCP submission for the current network generation.".to_owned();
            }
        }
        if sdk.is_some() {
            let empty_labels = BTreeMap::new();
            // Require the shared device while any active attachment targets it, so absence
            // cannot terminate only some attachments on a DPU that still needs placement.
            let requires_device = configurations.iter().any(|config| {
                desired_dpus(
                    config,
                    &instance,
                    &attached,
                    used.as_deref(),
                    host.host_snapshot.primary_attached_dpu_machine_id(),
                )
                .is_some_and(|required| required.contains(dpu_id))
            });
            observation.extension_service_statuses = configurations
                .iter()
                .map(|config| {
                    let required = desired_dpus(
                        config,
                        &instance,
                        &attached,
                        used.as_deref(),
                        host.host_snapshot.primary_attached_dpu_machine_id(),
                    );
                    let networked = instance
                        .config
                        .network
                        .service_interfaces
                        .iter()
                        .any(|endpoint| config.id == Some(endpoint.attachment_id));
                    // An absent device proves only nonnetworked label withdrawal when no active
                    // attachment needs it; networked endpoints still require their retained Node,
                    // exclusion and network evidence.
                    let labels = readback.map(|readback| &readback.node_labels).or_else(|| {
                        (!networked
                            && !requires_device
                            && missing_devices.contains(dpu_id)
                            && required
                                .as_ref()
                                .is_some_and(|required| !required.contains(dpu_id)))
                        .then_some(&empty_labels)
                    });
                    let error = errors
                        .get(dpu_id)
                        .map(String::as_str)
                        .unwrap_or("Placement evidence is unavailable.");
                    super::dpf_helm_chart_placement_status(
                        config,
                        required
                            .as_ref()
                            .is_some_and(|required| required.contains(dpu_id)),
                        instance.deleted.as_ref(),
                        labels
                            .map(PlacementEvidence::Verified)
                            .unwrap_or(PlacementEvidence::Error(error)),
                    )
                })
                .collect();
        }
        db::machine::update_extension_service_status_observation(
            txn.as_mut(),
            dpu_id,
            ExtensionServiceType::DpfHelmChart,
            observation,
        )
        .await?
        .check_applied()?;
    }
    txn.commit()
        .await
        .map_err(StateHandlerError::from_transaction)?;
    if !current_pass {
        return Ok(true);
    }
    if let Some(current) = host.instance.as_mut() {
        for (dpu_id, observation) in &observations {
            current
                .observations
                .extension_services
                .entry(*dpu_id)
                .or_default()
                .set_for_service_type(ExtensionServiceType::DpfHelmChart, observation.clone());
        }
    }

    // Avoid address locks when no DPF attachment is retiring.
    if !configurations.iter().any(|config| config.removed.is_some()) {
        return Ok(false);
    }

    // Lock addresses before the Instance, revalidate every proof and shared reference, and reclaim
    // at most one attachment so the next iteration observes the newly advanced versions.
    let mut txn = pool
        .txn_begin()
        .await
        .map_err(StateHandlerError::from_transaction)?;
    let Some(locked) = lock_reconciliation_state(txn.as_mut(), &instance, true).await? else {
        txn.commit()
            .await
            .map_err(StateHandlerError::from_transaction)?;
        return Ok(true);
    };
    if !matches_instance(&locked.instance, &instance)
        || locked.host.network_config.version != host.host_snapshot.network_config.version
        || locked.host.state.value != host.managed_state
    {
        txn.commit()
            .await
            .map_err(StateHandlerError::from_transaction)?;
        return Ok(true);
    }
    let current = &locked.instance;
    let live_instance = db::instance::find_by_id(txn.as_mut(), instance.id)
        .await?
        .ok_or_else(|| {
            StateHandlerError::GenericError(eyre::eyre!("instance disappeared while locked"))
        })?;
    for config in configurations
        .iter()
        .filter(|config| config.removed.is_some())
    {
        let Some(attachment_id) = config.id else {
            continue;
        };
        let endpoints = instance
            .config
            .network
            .service_interfaces
            .iter()
            .filter(|endpoint| config.id == Some(endpoint.attachment_id))
            .cloned()
            .collect::<Vec<_>>();
        if endpoints.is_empty() {
            continue;
        }
        let mut eligible = true;
        for endpoint in &endpoints {
            let Some(dpu) = locked.dpus.get(&endpoint.dpu_id) else {
                eligible = false;
                break;
            };
            let evidence = dpu
                .status
                .extension_service_status_observations
                .for_service_type(ExtensionServiceType::DpfHelmChart)
                .and_then(|observation| {
                    observation.service_vpc_endpoints.iter().find(|evidence| {
                        evidence.matches_endpoint(
                            endpoint,
                            config,
                            instance.extension_services_config_version,
                            instance.network_config_version,
                        )
                    })
                });
            if !evidence.is_some_and(|evidence| {
                evidence.deployment_status() == model::instance::status::extension_service::ExtensionServiceDeploymentStatus::Terminated
                    && locked.selectors.get(&(
                        config.service_id, endpoint.interface_ordinal, endpoint.slot_index,
                    )).is_some_and(|selector| {
                        evidence.matches_current_context(
                            &locked.host, &locked.host.state.value, &instance, dpu, selector,
                        )
                    })
            }) {
                eligible = false;
                break;
            }
        }
        if !eligible {
            continue;
        }
        let mut retained_host_memberships = super::super::vpc_dpu_memberships_for_interfaces(
            &current.network_config.interfaces,
            host,
            txn.as_mut(),
        )
        .await?;
        if let Some(update) = &live_instance.update_network_config_request {
            retained_host_memberships.extend(
                super::super::vpc_dpu_memberships_for_interfaces(
                    &update.old_config.interfaces,
                    host,
                    txn.as_mut(),
                )
                .await?,
            );
            retained_host_memberships.extend(
                super::super::vpc_dpu_memberships_for_interfaces(
                    &update.new_config.interfaces,
                    host,
                    txn.as_mut(),
                )
                .await?,
            );
        }
        let outcome = db::instance::reclaim_service_vpc_attachment(
            txn.as_mut(),
            instance.id,
            attachment_id,
            instance.extension_services_config_version,
            instance.network_config_version,
            &endpoints,
            common_pools.map(
                |common_pools| db::instance::ServiceVpcLoopbackReleaseContext {
                    common_pools,
                    retained_host_memberships: &retained_host_memberships,
                },
            ),
        )
        .await?;
        if matches!(outcome, ConditionalWrite::Applied(())) {
            txn.commit()
                .await
                .map_err(StateHandlerError::from_transaction)?;
            return Ok(true);
        }
    }
    txn.commit()
        .await
        .map_err(StateHandlerError::from_transaction)?;
    Ok(false)
}

/// Prepares identity only for service endpoint DPUs before reading a revision for fenced placement,
/// preserving label-only placement on the host's other DPUs.
///
/// Callers must supply the complete persisted service-interface DPU set in `endpoint_dpus`.
async fn read_dpu_placement(
    sdk: Option<&dyn DpfOperations>,
    dpu: &DpuMachine,
    endpoint_dpus: &BTreeSet<DpuMachineId>,
) -> Result<DpuDevicePlacementObservation, DpfError> {
    // Resolve the SDK and raw device ID before starting any management operation.
    let sdk = sdk.ok_or_else(|| DpfError::InvalidState("DPF is unavailable".to_owned()))?;
    let device = dpu
        .dpf_id()
        .ok_or_else(|| DpfError::InvalidState("DPU discovery has no dpf_id".to_owned()))?;
    let expected_dpu_id = endpoint_dpus.contains(&dpu.id).then_some(dpu.id);

    // Capture the revision after endpoint identity backfill so its resourceVersion fences the placement write.
    if let Some(dpu_id) = expected_dpu_id {
        sdk.ensure_dpu_device_node_identity(&device, dpu_id).await?;
    }
    sdk.get_dpu_device_placement(&device, expected_dpu_id).await
}

/// Runs placement attempts in four-DPU waves so unavailable DPUs leave time for queued attempts.
///
/// Each attempt receives at most the phase's initial remaining time divided by its number of waves.
/// `None` means that allowance expired or the attempt could not start before `deadline`.
async fn collect_placement_attempts<F: Future>(
    attempts: Vec<(DpuMachineId, F)>,
    deadline: Instant,
) -> Vec<(DpuMachineId, Option<F::Output>)> {
    // Empty placement has no wave to budget and performs no external work.
    if attempts.is_empty() {
        return Vec::new();
    }
    let concurrency = 4;
    let waves = u32::try_from(attempts.len().div_ceil(concurrency)).unwrap_or(u32::MAX);
    let attempt_time = deadline.saturating_duration_since(Instant::now()) / waves;

    // Start each allowance when a concurrency slot opens so slow first waves cannot starve later DPUs.
    let attempts = attempts
        .into_iter()
        .map(|(dpu_id, attempt)| async move {
            let started_at = Instant::now();
            let attempt_deadline = deadline.min(started_at + attempt_time);
            // Skip expired queued work because Tokio may poll a ready operation before its timeout.
            let completed = if started_at >= attempt_deadline {
                None
            } else {
                tokio::time::timeout_at(attempt_deadline, attempt)
                    .await
                    .ok()
            };
            (dpu_id, completed)
        })
        .collect::<Vec<_>>();
    stream::iter(attempts)
        .buffer_unordered(concurrency)
        .collect()
        .await
}

/// Compares both complete configurations as well as their versions to reject legacy writers that
/// preserve a token while changing JSON during a reconciliation attempt.
fn matches_instance(
    current: &db::instance::InstanceForUpdate,
    instance: &InstanceSnapshot,
) -> bool {
    current.machine_id == instance.machine_id
        && current.deleted == instance.deleted
        && current.extension_services_config_version == instance.extension_services_config_version
        && current.network_config_version == instance.network_config_version
        && current.extension_services_config == instance.config.extension_services
        && current.network_config == instance.config.network
}

/// Resolves networked service placement from its fixed endpoint DPUs and preserves existing policy
/// resolution for nonnetworked services so host-network changes cannot narrow accepted endpoints.
fn desired_dpus(
    config: &model::instance::config::extension_services::InstanceExtensionServiceConfig,
    instance: &InstanceSnapshot,
    attached: &[DpuMachineId],
    used: Option<&[DpuMachineId]>,
    primary: Option<DpuMachineId>,
) -> Option<Vec<DpuMachineId>> {
    let endpoints = instance
        .config
        .network
        .service_interfaces
        .iter()
        .filter(|endpoint| config.id == Some(endpoint.attachment_id))
        .map(|endpoint| endpoint.dpu_id)
        .collect::<BTreeSet<_>>();
    if config.removed.is_some() || instance.deleted.is_some() {
        // Keep unresolved nonnetworked labels while allowing fixed service endpoints to retire
        // without dpu_target.
        return (config.dpu_target.is_some() || !endpoints.is_empty()).then(Vec::new);
    }
    if !endpoints.is_empty() {
        return Some(endpoints.into_iter().collect());
    }
    config.dpu_target.and_then(|policy| {
        // Only AllActive needs host-network mapping; explicit placement remains independent.
        if policy == model::extension_service::DpuTarget::AllActive && used.is_none() {
            return None;
        }
        policy
            .resolve(attached, primary, used.unwrap_or_default())
            .ok()
    })
}

/// Builds the complete NICo service-label delta while retaining labels for unresolved nonnetworked
/// policies and every label belonging to another controller.
#[allow(clippy::too_many_arguments)]
fn placement_changes(
    instance: &InstanceSnapshot,
    configurations: &[&model::instance::config::extension_services::InstanceExtensionServiceConfig],
    dpu_id: DpuMachineId,
    before: &DpuDevicePlacementObservation,
    observation: Option<&InstanceExtensionServiceStatusObservation>,
    attached: &[DpuMachineId],
    used: Option<&[DpuMachineId]>,
    primary: Option<DpuMachineId>,
) -> BTreeMap<String, Option<String>> {
    let targets = configurations
        .iter()
        .filter_map(|config| {
            desired_dpus(config, instance, attached, used, primary)
                .map(|targets| (config.service_id, targets))
        })
        .collect::<HashMap<_, _>>();
    let unresolved = configurations
        .iter()
        .filter(|config| !targets.contains_key(&config.service_id))
        .map(|config| config.service_id)
        .collect::<BTreeSet<_>>();
    // Recognize only generated placement keys so similarly prefixed metadata remains untouched.
    let mut changes = before
        .node_labels
        .keys()
        .filter(|key| {
            let Some(suffix) = key.strip_prefix("nico/extsvc-") else {
                return false;
            };
            let Some(service_id) = suffix
                .get(..36)
                .and_then(|value| value.parse::<ExtensionServiceId>().ok())
            else {
                return false;
            };
            if unresolved.contains(&service_id) {
                return false;
            }
            let identity = DpfHelmChartIdentity::from_service_id(service_id);
            if **key == identity.placement_label_key {
                return true;
            }
            suffix
                .get(36..)
                .and_then(|local| local.strip_prefix("-if"))
                .and_then(|local| local.split_once("-slot"))
                .and_then(|(ordinal, slot)| {
                    Some((ordinal.parse::<u32>().ok()?, slot.parse::<u32>().ok()?))
                })
                .is_some_and(|(ordinal, slot)| {
                    **key == identity.slot_placement_label_key(ordinal, slot)
                })
        })
        .map(|key| (key.clone(), None))
        .collect::<BTreeMap<_, _>>();
    for config in configurations {
        let Some(targets) = targets.get(&config.service_id) else {
            continue;
        };
        let identity = DpfHelmChartIdentity::from_service_id(config.service_id);
        let endpoints = observation
            .into_iter()
            .flat_map(|observation| &observation.service_vpc_endpoints)
            .filter(|evidence| config.id == Some(evidence.endpoint.attachment_id))
            .collect::<Vec<_>>();
        let active = targets.contains(&dpu_id)
            && (endpoints.is_empty() || endpoints.iter().all(|evidence| evidence.node.is_some()));
        changes.insert(
            identity.placement_label_key.clone(),
            active.then(|| DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned()),
        );
        for evidence in endpoints {
            changes.insert(
                identity.slot_placement_label_key(
                    evidence.endpoint.interface_ordinal,
                    evidence.endpoint.slot_index,
                ),
                (active && evidence.context.intent == ServiceVpcChainIntent::Active)
                    .then(|| DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned()),
            );
        }
    }
    changes
}

/// Retains locked Instance, host, DPU and selector records so publication and reclamation
/// validate the same data while concurrent network writers wait for the transaction to finish.
struct LockedReconciliationState {
    instance: db::instance::InstanceForUpdate,
    host: HostMachine,
    dpus: BTreeMap<DpuMachineId, DpuMachine>,
    selectors: BTreeMap<(ExtensionServiceId, u32, u32), ExtensionServiceChainSelector>,
}

/// Locks Instance, host, all required DPUs and selectors in their shared order to prevent
/// publication and reclamation from reversing network-writer or sibling selector locks.
/// Reclamation also locks addresses before the Instance so cleanup cannot reverse IPAM writers.
///
/// Callers must enter without holding any of these row locks and retain the transaction through
/// their conditional writes. A changed Instance remains available for sticky bypass preservation.
async fn lock_reconciliation_state(
    txn: &mut sqlx::PgConnection,
    instance: &InstanceSnapshot,
    reclaiming: bool,
) -> Result<Option<LockedReconciliationState>, StateHandlerError> {
    // Lock addresses only for reclamation; every phase then locks Instance before host and DPUs.
    let current = if reclaiming {
        db::instance::find_by_id_for_service_vpc_reclamation(txn, instance.id).await?
    } else {
        db::instance::find_by_id_for_update(txn, instance.id).await?
    };
    let Some(current) = current else {
        return Ok(None);
    };
    // Lock separately before reading: find_one's FOR UPDATE locks a different table reference
    // and can return stale JSON whose host version still matches an old DPU acknowledgement.
    db::machine::lock_by_id_for_update(&mut *txn, &instance.machine_id.into()).await?;
    let Some(host) = db::machine::find_one(
        &mut *txn,
        &instance.machine_id,
        MachineSearchConfig::default(),
    )
    .await?
    else {
        return Ok(None);
    };

    // Include old and current endpoint DPUs so topology or desire changes cannot narrow locks.
    let endpoint_dpus = instance
        .config
        .network
        .service_interfaces
        .iter()
        .chain(&current.network_config.service_interfaces)
        .map(|endpoint| endpoint.dpu_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    db::instance::lock_service_vpc_dpus(txn, host.id, &endpoint_dpus).await?;
    let observed_dpus = host
        .associated_dpu_machine_ids()
        .into_iter()
        .chain(endpoint_dpus)
        .collect::<BTreeSet<_>>();
    let mut dpus = BTreeMap::new();
    for dpu_id in observed_dpus {
        if let Some(dpu) =
            db::machine::find_one(&mut *txn, &dpu_id, MachineSearchConfig::default()).await?
        {
            dpus.insert(dpu_id, dpu);
        }
    }

    // Prelock the full selector set once, after every DPU, before touching any shared exclusion.
    let service_ids = instance
        .config
        .extension_services
        .service_configs
        .iter()
        .chain(&current.extension_services_config.service_configs)
        .map(|config| config.service_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let selectors = db::extension_service::find_chain_selectors_for_update(txn, &service_ids)
        .await?
        .into_iter()
        .map(|selector| {
            (
                (
                    selector.service_id,
                    selector.interface_ordinal,
                    selector.slot,
                ),
                selector,
            )
        })
        .collect();
    Ok(Some(LockedReconciliationState {
        instance: current,
        host,
        dpus,
        selectors,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use carbide_uuid::machine::{DpuMachineId, MachineId, MachineIdSource, MachineType};
    use futures::FutureExt as _;

    use super::{Instant, collect_placement_attempts};

    /// Prepares identity before reading endpoint placement and stops on preparation failure, while
    /// other DPUs read labels directly so mixed attachments retain nonnetworked placement behavior.
    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn placement_reads_require_identity_only_for_endpoint_dpus() {
        use std::collections::{BTreeMap, BTreeSet};

        use carbide_dpf::{DpfError, DpuDevicePlacementObservation, ServiceVpcResourceObservation};
        use model::test_support::machine_snapshot::{dpu_machine, dpu_machine_id};

        use crate::dpf::MockDpfOperations;

        let cases = [
            // A mixed host's other endpoint cannot impose Node identity on this DPU's legacy labels.
            ("nonnetworked DPU", false, false),
            // Endpoint placement must capture its device revision after identity preparation succeeds.
            ("endpoint DPU", true, false),
            // A failed prerequisite must not yield placement that could authorize endpoint activation.
            ("endpoint preparation fails", true, true),
        ];
        for (scenario, has_endpoint, preparation_fails) in cases {
            // Vary endpoint membership and preparation failure; the fixture supplies a valid raw device ID.
            let dpu = dpu_machine(0);
            let dpu_id = dpu.id;
            let device = dpu.dpf_id().expect("fixture device ID");
            let endpoint_dpus = BTreeSet::from([if has_endpoint {
                dpu_id
            } else {
                dpu_machine_id(1)
            }]);
            let expected_dpu_id = has_endpoint.then_some(dpu_id);
            let mut sdk = MockDpfOperations::new();
            let mut sequence = mockall::Sequence::new();

            // Require preparation only for endpoint DPUs, before any successful placement read.
            if has_endpoint {
                let expected_device = device.clone();
                sdk.expect_ensure_dpu_device_node_identity()
                    .withf(move |device, identity| device == expected_device && *identity == dpu_id)
                    .times(1)
                    .in_sequence(&mut sequence)
                    .returning(move |_, _| {
                        if preparation_fails {
                            Err(DpfError::InvalidState(
                                "injected identity conflict".to_owned(),
                            ))
                        } else {
                            Ok(())
                        }
                    });
            }
            // Return a recognizable revision only after the required preparation has succeeded.
            if !preparation_fails {
                let expected_device = device.clone();
                sdk.expect_get_dpu_device_placement()
                    .withf(move |device, identity| {
                        device == expected_device && *identity == expected_dpu_id
                    })
                    .times(1)
                    .in_sequence(&mut sequence)
                    .return_once(move |_, _| {
                        Ok(DpuDevicePlacementObservation {
                            resource: ServiceVpcResourceObservation {
                                namespace: "service-vpc".to_owned(),
                                name: device,
                                uid: "observed-device".to_owned(),
                                generation: 1,
                                resource_version: "17".to_owned(),
                                is_deleting: false,
                            },
                            node_labels: BTreeMap::new(),
                            config_version: None,
                        })
                    });
            }

            // Run the same preparation/read operation used by the controller's per-DPU attempts.
            let result = super::read_dpu_placement(Some(&sdk), &dpu, &endpoint_dpus).await;

            // Preserve the preparation failure or the actual observed revision without inventing placement.
            if preparation_fails {
                assert!(
                    matches!(result, Err(DpfError::InvalidState(message))
                    if message == "injected identity conflict"),
                    "{scenario}"
                );
            } else {
                let observed = result.expect(scenario);
                assert_eq!(observed.resource.uid, "observed-device", "{scenario}");
                assert_eq!(observed.resource.resource_version, "17", "{scenario}");
            }
        }
    }

    /// Builds five distinct DPU IDs so queued work exceeds the four-attempt concurrency limit.
    fn placement_dpus() -> [DpuMachineId; 5] {
        // Vary the hardware hash while retaining the DPU machine type required by placement callers.
        std::array::from_fn(|index| {
            MachineId::new(MachineIdSource::Tpm, [index as u8; 32], MachineType::Dpu)
                .try_into()
                .expect("DPU fixture ID")
        })
    }

    /// Preserves a removed nonnetworked attachment's labels when its target is absent while
    /// retiring resolved siblings, so service-VPC reconciliation retains legacy placement behavior.
    #[test]
    fn placement_changes_preserve_unresolved_legacy_attachment_labels() {
        use std::collections::{BTreeMap, HashMap};

        use carbide_dpf::{DpuDevicePlacementObservation, ServiceVpcResourceObservation};
        use carbide_uuid::extension_service::ExtensionServiceId;
        use config_version::ConfigVersion;
        use model::extension_service::{DpfHelmChartIdentity, DpuTarget};
        use model::instance::config::InstanceConfig;
        use model::instance::config::extension_services::{
            InstanceExtensionServiceConfig, InstanceExtensionServicesConfig,
        };
        use model::instance::config::network::{
            InstanceNetworkConfig, InstanceServiceInterfaceConfig,
        };
        use model::instance::config::tenant_config::TenantConfig;
        use model::instance::snapshot::InstanceSnapshot;
        use model::instance::status::InstanceStatusObservations;
        use model::os::{InlineIpxe, OperatingSystem, OperatingSystemVariant};
        use model::test_support::machine_snapshot::{dpu_machine_id, host_machine_id};

        // Vary target and endpoint availability; the remaining fields supply valid attachment identities.
        let dpu_id = dpu_machine_id(0);
        let networked_attachment = uuid::Uuid::new_v4();
        let removed_at = chrono::Utc::now();
        let version = ConfigVersion::initial();
        let service_configs = [
            // An unresolved nonnetworked attachment keeps its labels during retirement.
            (Some(uuid::Uuid::new_v4()), None, Some(removed_at)),
            // Fixed endpoints permit retirement even when the attachment's dpu_target is absent.
            (Some(networked_attachment), None, Some(removed_at)),
            // A recorded target permits nonnetworked retirement independently of network mapping.
            (
                Some(uuid::Uuid::new_v4()),
                Some(DpuTarget::All),
                Some(removed_at),
            ),
            // A resolved active sibling still receives its desired placement label.
            (Some(uuid::Uuid::new_v4()), Some(DpuTarget::All), None),
        ]
        .map(|(id, dpu_target, removed)| InstanceExtensionServiceConfig {
            id,
            service_id: ExtensionServiceId::new(),
            version,
            dpu_target,
            removed,
        });
        let [legacy_label, networked_label, targeted_label, active_label] =
            service_configs.each_ref().map(|config| {
                DpfHelmChartIdentity::from_service_id(config.service_id).placement_label_key
            });
        let slot_label = DpfHelmChartIdentity::from_service_id(service_configs[1].service_id)
            .slot_placement_label_key(0, 0);

        // Supply one fixed endpoint; the unrelated Instance fields satisfy the snapshot type.
        let instance = InstanceSnapshot {
            id: uuid::Uuid::new_v4().into(),
            machine_id: host_machine_id().into(),
            instance_type_id: None,
            metadata: Default::default(),
            config: InstanceConfig {
                tenant: TenantConfig {
                    tenant_organization_id: "tenant-a".parse().expect("valid tenant ID"),
                    tenant_keyset_ids: Vec::new(),
                    hostname: None,
                },
                os: OperatingSystem {
                    user_data: None,
                    variant: OperatingSystemVariant::Ipxe(InlineIpxe {
                        ipxe_script: "boot".to_owned(),
                    }),
                    phone_home_enabled: false,
                    run_provisioning_instructions_on_every_boot: false,
                },
                network: InstanceNetworkConfig {
                    service_interfaces: vec![InstanceServiceInterfaceConfig {
                        attachment_id: networked_attachment, // Match only the networked sibling.
                        interface_ordinal: 0,
                        dpu_id, // Fix the networked service's placement on this DPU.
                        slot_index: 0,
                        vpc_id: carbide_uuid::vpc::VpcId::new(),
                        vpc_prefix_id: carbide_uuid::vpc::VpcPrefixId::new(),
                        network_segment_id: carbide_uuid::network::NetworkSegmentId::new(),
                        network_prefix_id: carbide_uuid::network::NetworkPrefixId::new(),
                        link_prefix: "192.0.2.0/31".parse().expect("valid service prefix"),
                        mac_address: mac_address::MacAddress::new([2, 0, 0, 0, 0, 1]),
                        internal_uuid: uuid::Uuid::new_v4(),
                    }],
                    ..Default::default()
                },
                infiniband: Default::default(),
                network_security_group_id: None,
                dhcp_option_set_id: None,
                extension_services: InstanceExtensionServicesConfig {
                    service_configs: service_configs.into(),
                },
                nvlink: Default::default(),
                spxconfig: Default::default(),
                power_profile: None,
            },
            nic_firmware_snapshot: None,
            config_version: version,
            network_config_version: version,
            ib_config_version: version,
            storage_config_version: version,
            extension_services_config_version: version,
            nvlink_config_version: version,
            spx_config_version: version,
            observations: InstanceStatusObservations {
                network: HashMap::new(),
                extension_services: HashMap::new(),
                phone_home_last_contact: None,
            },
            use_custom_pxe_on_boot: false,
            custom_pxe_reboot_requested: false,
            deleted: None, // Keep the Instance live for the active sibling's placement check.
            update_network_config_request: None,
        };
        // Seed placed siblings and unrelated metadata at one observed DPUDevice revision.
        let before = DpuDevicePlacementObservation {
            resource: ServiceVpcResourceObservation {
                namespace: "service-vpc".to_owned(),
                name: "device".to_owned(),
                uid: "device-uid".to_owned(),
                generation: 1,
                resource_version: "1".to_owned(),
                is_deleting: false,
            },
            node_labels: [
                legacy_label,
                networked_label.clone(),
                targeted_label.clone(),
                slot_label.clone(),
                "other-controller".to_owned(),
            ]
            .map(|key| (key, super::DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned()))
            .into(),
            config_version: None,
        };

        // Compute the complete delta without host-network mapping to exercise independent retirement.
        let configurations = instance
            .config
            .extension_services
            .service_configs
            .iter()
            .collect::<Vec<_>>();
        let changes = super::placement_changes(
            &instance,
            &configurations,
            dpu_id,
            &before,
            None,
            &[dpu_id],
            None,
            Some(dpu_id),
        );

        // Retire both resolved siblings and their slot while leaving legacy and unrelated labels untouched.
        assert_eq!(
            changes,
            BTreeMap::from([
                (networked_label, None),
                (targeted_label, None),
                (slot_label, None),
                (
                    active_label,
                    Some(super::DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_owned())
                ),
            ]),
        );
    }

    /// Retains a fifth healthy DPU's result after four pending attempts exhaust their first-wave
    /// allowances, preventing fixed DPU order from starving healthy placement work on every retry.
    #[tokio::test(start_paused = true)]
    async fn placement_deadlines_allow_healthy_dpus_after_a_stalled_wave() {
        // Queue one healthy DPU behind four pending attempts to exercise the second wave.
        let dpus = placement_dpus();
        let healthy_dpu = dpus[4];
        let started = Arc::new(Mutex::new(Vec::new()));
        let attempts = dpus
            .into_iter()
            .map(|dpu_id| {
                let started = started.clone();
                (dpu_id, async move {
                    started.lock().expect("started attempts").push(dpu_id);
                    if dpu_id == healthy_dpu {
                        true
                    } else {
                        std::future::pending().await
                    }
                })
            })
            .collect();
        let phase_started_at = Instant::now();
        let deadline = phase_started_at + Duration::from_secs(8);
        let work = collect_placement_attempts(attempts, deadline);
        tokio::pin!(work);

        // Keep the first wave pending until its four-second share expires.
        assert!(work.as_mut().now_or_never().is_none());
        assert_eq!(started.lock().expect("started attempts").len(), 4);
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(work.as_mut().now_or_never().is_none());
        tokio::time::advance(Duration::from_secs(1)).await;
        let mut completed = work.await;

        // Every DPU keeps its own result, including the healthy DPU behind the stalled first wave.
        completed.sort_by_key(|(dpu_id, _)| *dpu_id);
        let mut expected = dpus.map(|dpu_id| (dpu_id, (dpu_id == healthy_dpu).then_some(true)));
        expected.sort_by_key(|(dpu_id, _)| *dpu_id);
        assert_eq!(completed.as_slice(), expected.as_slice());
        // Completing the second wave must leave time inside the phase for subsequent controller work.
        assert_eq!(phase_started_at.elapsed(), Duration::from_secs(4));
        assert!(Instant::now() < deadline);
    }

    /// Skips a queued DPU when the task resumes after phase expiry, preventing late polling from
    /// launching new external work after the placement phase deadline.
    #[tokio::test(start_paused = true)]
    async fn placement_deadlines_do_not_start_queued_work_after_expiry() {
        // Build five pending futures so one can wait behind the four-attempt concurrency limit.
        let dpus = placement_dpus();
        let queued_dpu = dpus[4];
        let started = Arc::new(Mutex::new(Vec::new()));
        let attempts = dpus
            .into_iter()
            .map(|dpu_id| {
                let started = started.clone();
                (dpu_id, async move {
                    started.lock().expect("started attempts").push(dpu_id);
                    std::future::pending::<()>().await
                })
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(8);
        let work = collect_placement_attempts(attempts, deadline);
        tokio::pin!(work);

        // Resume the task after the whole phase instead of polling it when the first wave expires.
        assert!(work.as_mut().now_or_never().is_none());
        tokio::time::advance(Duration::from_secs(8)).await;
        let completed = work.await;

        // No fifth operation starts, and every result explicitly remains incomplete.
        let started_attempts = started.lock().expect("started attempts");
        assert_eq!(started_attempts.len(), 4);
        assert!(!started_attempts.contains(&queued_dpu));
        assert_eq!(completed.len(), 5);
        assert!(completed.into_iter().all(|(_, result)| result.is_none()));
    }
}
