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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use carbide_dpf::DpuDeploymentType;
use carbide_network::ip::IdentifyAddressFamily;
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::DpuMachineId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use chrono::{DateTime, Utc};
use db::ObjectColumnFilter;
use model::ConfigValidationError;
use model::extension_service::{
    ExtensionService, ExtensionServiceLifecycleState, ExtensionServiceType, ServiceVpcAddressFamily,
};
use model::instance::config::extension_services::{
    InstanceExtensionServicesConfig, RequestedInstanceExtensionServicesConfig,
};
use model::instance::config::network::{
    DeviceLocator, InstanceNetworkConfig, InstanceServiceInterfaceConfig, InterfaceFunctionId,
    NetworkDetails,
};
use model::instance::snapshot::InstanceSnapshot;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{InstanceState, ManagedHostState, ManagedHostStateSnapshot};
use model::network_segment::{AllocationStrategy, NetworkSegmentSearchConfig, NetworkSegmentType};
use model::vpc::vpc_prefix_can_allocate_interface_prefix;
use model::vpc_prefix::VpcPrefix;

use crate::api::Api;
use crate::network_segment::allocate::PrefixAllocator;
use crate::{CarbideError, CarbideResult};

#[derive(Clone, Copy)]
struct EndpointIntent {
    service_id: ExtensionServiceId,
    attachment_id: uuid::Uuid,
    interface_ordinal: u32,
    dpu_id: DpuMachineId,
    vpc_id: VpcId,
    family: ServiceVpcAddressFamily,
    mac_address: mac_address::MacAddress,
}

/// Distinguishes requests requiring no new service interface from requests that must create network
/// segments, network prefixes, Instance addresses, and service-interface entries.
enum ServiceInterfaceAllocationPlan {
    NoNewServiceInterfaces,
    Allocate {
        intents: Vec<EndpointIntent>,
        device_to_id: HashMap<String, Vec<DpuMachineId>>,
        primary_dpu: Option<DpuMachineId>,
    },
}

/// Couples an allocated endpoint with its link prefix so the caller advances the in-memory
/// VPC-prefix cursor only after the savepoint commits.
struct ServiceCandidateAllocation {
    interface: InstanceServiceInterfaceConfig,
    link_prefix: ipnetwork::IpNetwork,
}

/// Returns whether this binary supports the complete service-VPC lifecycle for an address family so
/// operator configuration cannot expose a family missing a required reconciliation path.
pub(crate) const fn family_implemented(_family: ServiceVpcAddressFamily) -> bool {
    false
}

/// Rejects a newly attached networked service unless its required family has complete lifecycle
/// support, applying the same fail-closed admission rule to create and update.
pub(crate) fn validate_implementation_support(service: &ExtensionService) -> CarbideResult<()> {
    #[cfg(test)]
    if test_support::implementation_support_enabled() {
        return Ok(());
    }

    if service
        .service_vpc_interfaces
        .iter()
        .any(|requirement| !family_implemented(requirement.address_family))
    {
        return Err(CarbideError::FailedPrecondition(
            "service VPC attachment is unavailable until network resource reconciliation is implemented"
                .to_string(),
        ));
    }
    Ok(())
}

/// Marks every active attachment referenced by a service interface as removed in one timestamped
/// update so whole-instance release preserves service-interface records until cleanup is observed.
pub(crate) fn terminate_networked_attachments(
    current: &InstanceExtensionServicesConfig,
    network: &InstanceNetworkConfig,
    removed_at: DateTime<Utc>,
) -> Option<InstanceExtensionServicesConfig> {
    let networked_ids = network
        .service_interfaces
        .iter()
        .map(|interface| interface.attachment_id)
        .collect::<HashSet<_>>();
    let mut updated = current.clone();
    let mut changed = false;
    for attachment in &mut updated.service_configs {
        if attachment.removed.is_none()
            && attachment
                .id
                .is_some_and(|attachment_id| networked_ids.contains(&attachment_id))
        {
            attachment.removed = Some(removed_at);
            changed = true;
        }
    }
    changed.then_some(updated)
}

/// Validates and allocates every new service interface inside the caller's Instance transaction so
/// attachment, service-interface, prefix, and address records become visible in one commit.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn allocate_for_instance(
    api: &Api,
    txn: &mut db::Transaction<'_>,
    instance_id: InstanceId,
    extension_services: &InstanceExtensionServicesConfig,
    requested: &RequestedInstanceExtensionServicesConfig,
    current_network: &InstanceNetworkConfig,
    host: &ManagedHostStateSnapshot,
    expected_machine_has_cx9: bool,
    expected_instance: Option<&InstanceSnapshot>,
) -> CarbideResult<InstanceNetworkConfig> {
    if expected_instance.is_some_and(|instance| instance.update_network_config_request.is_some()) {
        return Err(ConfigValidationError::InstanceNetworkConfigUpdateAlreadyInProgress.into());
    }

    // Resolve requested registrations and every service referenced by a persisted service interface
    // so retries validate its attachment, DPU, VPC, prefix, segment, address, and MAC fields first.
    let mut service_ids = requested
        .service_configs
        .iter()
        .map(|selection| selection.service_id)
        .collect::<BTreeSet<_>>();
    for endpoint in &current_network.service_interfaces {
        let mut owners = extension_services
            .service_configs
            .iter()
            .filter(|attachment| attachment.id == Some(endpoint.attachment_id));
        let owner = owners.next().ok_or_else(|| {
            CarbideError::FailedPrecondition(format!(
                "stored service interface {} has no attachment owner",
                endpoint.internal_uuid
            ))
        })?;
        if owners.next().is_some() {
            return Err(CarbideError::FailedPrecondition(format!(
                "stored service interface {} has more than one attachment owner",
                endpoint.internal_uuid
            )));
        }
        service_ids.insert(owner.service_id);
    }
    let service_ids = service_ids.into_iter().collect::<Vec<_>>();
    let (services, _) = super::load_extension_services(txn, &service_ids).await?;
    validate_persisted_endpoints(
        txn.as_mut(),
        extension_services,
        current_network,
        &services,
        api.runtime_config.dpu_config.service_vpc_slot_count,
        api.runtime_config
            .dpu_config
            .max_active_service_vpc_interfaces_per_dpu,
    )
    .await?;
    // Active attachment IDs determine which service-interface DPUs require an FNN host VPC during
    // host-network replacement. Removed attachments retain their persisted service interfaces
    // during cleanup but do not require an FNN host VPC on the replacement host network.
    let active_attachment_ids = extension_services
        .service_configs
        .iter()
        .filter(|attachment| attachment.removed.is_none())
        .filter_map(|attachment| attachment.id)
        .collect::<HashSet<_>>();
    let active_endpoint_dpus = current_network
        .service_interfaces
        .iter()
        .filter(|endpoint| active_attachment_ids.contains(&endpoint.attachment_id))
        .map(|endpoint| endpoint.dpu_id)
        .collect::<BTreeSet<_>>();
    let host_network_update_requested = expected_instance.is_some_and(|instance| {
        instance
            .config
            .network
            .is_network_config_update_requested(current_network)
    });
    // Build `ServiceInterfaceAllocationPlan` before taking address and VPC-prefix allocation locks.
    let ServiceInterfaceAllocationPlan::Allocate {
        intents,
        device_to_id,
        primary_dpu,
    } = plan_service_interface_allocation(
        api,
        txn,
        extension_services,
        requested,
        current_network,
        host,
        &services,
        expected_machine_has_cx9,
    )
    .await?
    else {
        validate_update_without_new_service_interfaces(
            txn,
            instance_id,
            current_network,
            host,
            &active_endpoint_dpus,
            host_network_update_requested,
        )
        .await?;
        return Ok(current_network.clone());
    };
    // Lock `instance_addresses` before reading VPC-backed host and service configurations so VPC
    // deletion either commits first or observes this transaction's new service-interface addresses.
    db::instance_address::lock_table_for_allocation(txn.as_mut()).await?;

    let overlap_network = resolve_candidate_host_vpcs(txn, current_network).await?;
    validate_new_attachment_overlap(
        api.runtime_config
            .dpu_config
            .allow_service_vpc_tenant_overlap,
        &overlap_network,
        expected_instance.map(|instance| &instance.config.network),
        &intents,
    )?;

    // Resolve every tenant-network VPC used by the target DPUs because service-VPC activation
    // requires FNN on each participating host and service VPC.
    let mut target_dpus = intents
        .iter()
        .map(|intent| intent.dpu_id)
        .collect::<BTreeSet<_>>();
    if host_network_update_requested {
        target_dpus.extend(active_endpoint_dpus);
    }
    let host_vpc_ids =
        host_vpc_ids_for_target_dpus(&overlap_network, &device_to_id, primary_dpu, &target_dpus)?;

    // Re-read every participating VPC while the `instance_addresses` lock serializes deletion.
    // `VpcUpdateVirtualization` requires allocation quiescence and has no concurrency guarantee;
    // locking a VPC here would invert deletion's VPC-before-address-table order.
    let service_vpc_ids = intents
        .iter()
        .map(|intent| intent.vpc_id)
        .collect::<BTreeSet<_>>();
    let all_vpc_ids = service_vpc_ids
        .iter()
        .copied()
        .chain(host_vpc_ids)
        .collect::<BTreeSet<_>>();
    validate_active_fnn_vpcs(txn, all_vpc_ids).await?;

    let vpc_ids = service_vpc_ids.into_iter().collect::<Vec<_>>();
    let prefix_candidates = db::vpc_prefix::find_allocation_candidates(txn.as_mut(), &vpc_ids)
        .await?
        .into_iter()
        .filter(|prefix| {
            let link_prefix_len = if prefix.config.prefix.is_ipv4() {
                31
            } else {
                127
            };
            vpc_prefix_can_allocate_interface_prefix(
                prefix.config.prefix.address_family(),
                prefix.config.prefix.prefix(),
                link_prefix_len,
            )
        })
        .fold(
            BTreeMap::<(VpcId, u8), Vec<VpcPrefixId>>::new(),
            |mut candidates, prefix| {
                let family = if prefix.config.prefix.is_ipv4() { 4 } else { 6 };
                candidates
                    .entry((prefix.vpc_id, family))
                    .or_default()
                    .push(prefix.id);
                candidates
            },
        );
    for intent in &intents {
        if service_prefix_candidates(&prefix_candidates, intent.vpc_id, intent.family).is_none() {
            return Err(CarbideError::FailedPrecondition(format!(
                "service VPC {} has no eligible {:?} prefix",
                intent.vpc_id, intent.family
            )));
        }
    }

    let assignments = assign_slots(
        &current_network.service_interfaces,
        &intents
            .iter()
            .map(|intent| (intent.dpu_id, intent.vpc_id))
            .collect::<Vec<_>>(),
        api.runtime_config.dpu_config.service_vpc_slot_count,
        api.runtime_config
            .dpu_config
            .max_active_service_vpc_interfaces_per_dpu,
    )?;

    validate_persisted_endpoint_addresses(txn.as_mut(), instance_id, current_network).await?;
    let mut locked_prefixes = HashMap::new();
    for prefix_id in prefix_candidates
        .values()
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>()
    {
        if let Some(prefix) = db::vpc_prefix::lock_for_allocation(txn.as_mut(), prefix_id).await? {
            locked_prefixes.insert(prefix_id, prefix);
        }
    }
    // Recheck each candidate group after locking so a VPC prefix deleted since discovery cannot be
    // reported as capacity exhaustion or redirect the request to another address family.
    for intent in &intents {
        let candidates =
            service_prefix_candidates(&prefix_candidates, intent.vpc_id, intent.family)
                .ok_or_else(|| {
                    CarbideError::internal(format!(
                        "validated service VPC {} lost its prefix candidates",
                        intent.vpc_id
                    ))
                })?;
        if !candidates
            .iter()
            .any(|candidate_id| locked_prefixes.contains_key(candidate_id))
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "service VPC {} has no active eligible {:?} prefix",
                intent.vpc_id, intent.family
            )));
        }
    }

    // After locking `instance_addresses` and candidate VPC-prefix rows, existing-Instance activation
    // locks the Instance row before the Machine row to serialize with host updates and the
    // force-deletion eligibility decision.
    if let Some(expected) = expected_instance {
        let locked = db::instance::find_by_id_for_update(txn.as_mut(), expected.id)
            .await?
            .ok_or(CarbideError::NotFoundError {
                kind: "instance",
                id: expected.id.to_string(),
            })?;
        let locked_snapshot = db::instance::find_by_id(&mut *txn, expected.id)
            .await?
            .ok_or(CarbideError::NotFoundError {
                kind: "instance",
                id: expected.id.to_string(),
            })?;
        // A non-null locked `deleted` timestamp means release has begun, so return
        // `InstanceDeletionIsRequested` instead of an optimistic-concurrency conflict.
        if locked.deleted.is_some() {
            return Err(ConfigValidationError::InstanceDeletionIsRequested.into());
        }
        if locked.extension_services_config_version != expected.extension_services_config_version
            || locked.network_config_version != expected.network_config_version
            || locked.extension_services_config != expected.config.extension_services
            || locked.network_config != expected.config.network
            || locked_snapshot.update_network_config_request
                != expected.update_network_config_request
        {
            return Err(CarbideError::ConcurrentModificationError(
                "instance",
                expected.network_config_version.to_string(),
            ));
        }
        let machine = db::machine::find_one(
            &mut *txn,
            &locked.machine_id,
            MachineSearchConfig {
                for_update: true,
                ..Default::default()
            },
        )
        .await?
        .ok_or(CarbideError::NotFoundError {
            kind: "machine",
            id: locked.machine_id.to_string(),
        })?;
        // Recheck the Machine's `ManagedHostState` while holding its row lock because reset,
        // reprovisioning, and maintenance can invalidate the earlier Ready value.
        if !matches!(
            machine.current_state(),
            ManagedHostState::Assigned {
                instance_state: InstanceState::Ready,
            }
        ) {
            return Err(CarbideError::FailedPrecondition(format!(
                "machine {} is no longer eligible for service VPC activation",
                locked.machine_id
            )));
        }
    }

    allocate_service_interfaces(
        txn,
        instance_id,
        current_network,
        intents,
        assignments,
        &prefix_candidates,
        &mut locked_prefixes,
    )
    .await
}

/// Builds a `ServiceInterfaceAllocationPlan` by validating requested networked attachments before
/// `allocate_for_instance` takes the `instance_addresses` or VPC-prefix allocation locks.
#[allow(clippy::too_many_arguments)]
async fn plan_service_interface_allocation(
    api: &Api,
    txn: &mut db::Transaction<'_>,
    extension_services: &InstanceExtensionServicesConfig,
    requested: &RequestedInstanceExtensionServicesConfig,
    current_network: &InstanceNetworkConfig,
    host: &ManagedHostStateSnapshot,
    services: &HashMap<ExtensionServiceId, ExtensionService>,
    expected_machine_has_cx9: bool,
) -> CarbideResult<ServiceInterfaceAllocationPlan> {
    // Apply service-VPC prerequisites only after ordinary extension services have been resolved.
    let mut has_networked_service = false;
    for selection in &requested.service_configs {
        let service =
            services
                .get(&selection.service_id)
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "extension service",
                    id: selection.service_id.to_string(),
                })?;
        if service.service_vpc_interfaces.is_empty() {
            if !selection.service_vpc_ids.is_empty() {
                return Err(CarbideError::InvalidArgument(format!(
                    "extension service {} declares no service VPC interfaces",
                    service.id
                )));
            }
        } else {
            has_networked_service = true;
        }
    }
    if !has_networked_service {
        return Ok(ServiceInterfaceAllocationPlan::NoNewServiceInterfaces);
    }

    if api.runtime_config.site_global_vpc_vni.is_some() {
        return Err(CarbideError::FailedPrecondition(
            "service VPCs require site_global_vpc_vni to be unset".to_string(),
        ));
    }
    if !host.host_snapshot.config.dpf.used_for_ingestion || host.dpu_snapshots.is_empty() {
        return Err(CarbideError::FailedPrecondition(
            "service VPCs require a DPF-managed host with attached DPUs".to_string(),
        ));
    }
    let dpf_sdk = api.dpf_sdk.as_ref().ok_or_else(|| {
        CarbideError::FailedPrecondition(
            "service VPCs require the DPF integration to be configured".to_string(),
        )
    })?;
    let (_, device_to_id) = host.host_snapshot.get_dpu_device_and_id_mappings()?;
    let primary_dpu = host.host_snapshot.primary_attached_dpu_machine_id();
    let attached_dpus = host
        .dpu_snapshots
        .iter()
        .map(|dpu| dpu.id)
        .collect::<Vec<_>>();
    let used_dpus = current_network.get_used_dpus(&device_to_id, primary_dpu);
    let enabled_families = api
        .runtime_config
        .dpf
        .deployments
        .service_vpc_address_families();

    let mut intents = Vec::new();
    for selection in &requested.service_configs {
        let service =
            services
                .get(&selection.service_id)
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "extension service",
                    id: selection.service_id.to_string(),
                })?;
        if service.service_vpc_interfaces.is_empty() {
            continue;
        }
        let attachment = extension_services
            .service_configs
            .iter()
            .find(|attachment| {
                attachment.service_id == selection.service_id
                    && attachment.version == selection.version
                    && attachment.removed.is_none()
            })
            .ok_or_else(|| {
                CarbideError::internal(format!(
                    "accepted service {} version {} has no active attachment",
                    selection.service_id, selection.version
                ))
            })?;
        let attachment_id = attachment.id.ok_or_else(|| {
            CarbideError::FailedPrecondition(format!(
                "networked extension-service attachment {} has no durable identity",
                selection.service_id
            ))
        })?;
        let stored_endpoints = current_network
            .service_interfaces
            .iter()
            .filter(|endpoint| endpoint.attachment_id == attachment_id)
            .collect::<Vec<_>>();
        // Reconstruct omitted VPC selections only from retained endpoints; new attachments supply them.
        let selected_vpcs = if selection.service_vpc_ids.is_empty() && !stored_endpoints.is_empty()
        {
            let mut stored_selections = BTreeMap::new();
            for endpoint in &stored_endpoints {
                if stored_selections
                    .insert(endpoint.interface_ordinal, endpoint.vpc_id)
                    .is_some_and(|vpc_id| vpc_id != endpoint.vpc_id)
                {
                    return Err(CarbideError::FailedPrecondition(format!(
                        "extension-service attachment {attachment_id} has conflicting stored VPC selections"
                    )));
                }
            }
            (0..service.service_vpc_interfaces.len())
                .map(|ordinal| {
                    u32::try_from(ordinal)
                        .ok()
                        .and_then(|ordinal| stored_selections.remove(&ordinal))
                        .ok_or_else(|| {
                            CarbideError::FailedPrecondition(format!(
                                "extension-service attachment {attachment_id} has incomplete stored VPC selections"
                            ))
                        })
                })
                .collect::<CarbideResult<Vec<_>>>()?
        } else {
            selection.service_vpc_ids.clone()
        };
        let new_attachment = stored_endpoints.is_empty();
        validate_service_registration(service, &selected_vpcs, new_attachment)?;
        let targets = service
            .dpu_target
            .ok_or_else(|| {
                CarbideError::FailedPrecondition(format!(
                    "DPF extension service {} has no DPU target policy",
                    service.id
                ))
            })?
            .resolve(&attached_dpus, primary_dpu, &used_dpus)
            .map_err(|error| CarbideError::FailedPrecondition(error.to_string()))?;
        let macs = db::extension_service::find_interface_macs(txn.as_mut(), service.id).await?;
        if macs.len() != service.service_vpc_interfaces.len() {
            return Err(CarbideError::FailedPrecondition(format!(
                "extension service {} registration has {} stable interface MACs but declares {} interfaces",
                service.id,
                macs.len(),
                service.service_vpc_interfaces.len()
            )));
        }

        let mut service_intents = Vec::new();
        for dpu_id in targets {
            let dpu = host
                .dpu_snapshots
                .iter()
                .find(|dpu| dpu.id == dpu_id)
                .ok_or_else(|| {
                    CarbideError::FailedPrecondition(format!(
                        "target DPU {dpu_id} is absent from the managed-host snapshot"
                    ))
                })?;
            let deployment = dpf_sdk
                .deployment_type_for_dpu(dpu, expected_machine_has_cx9)
                .map_err(|error| {
                    CarbideError::FailedPrecondition(format!(
                        "target DPU {dpu_id} has no supported DPF deployment: {error}"
                    ))
                })?;
            if new_attachment && deployment == DpuDeploymentType::Bf4Astra {
                return Err(CarbideError::FailedPrecondition(format!(
                    // xtask:allow-error-case: Astra is a deployment name
                    "target DPU {dpu_id} uses Astra, which does not support service VPCs"
                )));
            }
            for (ordinal, (requirement, vpc_id)) in service
                .service_vpc_interfaces
                .iter()
                .zip(&selected_vpcs)
                .enumerate()
            {
                if new_attachment
                    && !enabled_families
                        .get(&deployment)
                        .is_some_and(|families| families.contains(&requirement.address_family))
                {
                    return Err(CarbideError::FailedPrecondition(format!(
                        "service VPC {:?} is disabled for target DPU {dpu_id} deployment {deployment:?}",
                        requirement.address_family
                    )));
                }
                let interface_ordinal = u32::try_from(ordinal).map_err(|error| {
                    CarbideError::internal(format!(
                        "service interface ordinal {ordinal} cannot be represented: {error}"
                    ))
                })?;
                let mac_address = macs
                    .iter()
                    .find(|mac| mac.interface_ordinal == interface_ordinal)
                    .ok_or_else(|| {
                        CarbideError::FailedPrecondition(format!(
                            "extension service {} registration has no MAC for interface {interface_ordinal}",
                            service.id
                        ))
                    })?
                    .mac_address;
                service_intents.push(EndpointIntent {
                    service_id: service.id,
                    attachment_id,
                    interface_ordinal,
                    dpu_id,
                    vpc_id: *vpc_id,
                    family: requirement.address_family,
                    mac_address,
                });
            }
        }
        // Allocate only when no persisted service interfaces exist; retries preserve their stored fields.
        if stored_endpoints.is_empty() {
            intents.extend(service_intents);
        } else if stored_endpoints.len() != service_intents.len()
            || service_intents.iter().any(|intent| {
                !stored_endpoints.iter().any(|endpoint| {
                    endpoint.interface_ordinal == intent.interface_ordinal
                        && endpoint.dpu_id == intent.dpu_id
                        && endpoint.vpc_id == intent.vpc_id
                        && endpoint.mac_address == intent.mac_address
                        && endpoint.link_prefix.is_ipv4()
                            == matches!(intent.family, ServiceVpcAddressFamily::Ipv4)
                })
            })
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "extension-service attachment {attachment_id} does not match its immutable stored service interfaces"
            )));
        }
    }

    intents.sort_by_key(|intent| {
        (
            intent.service_id,
            intent.attachment_id,
            intent.dpu_id,
            intent.interface_ordinal,
        )
    });
    if intents.is_empty() {
        Ok(ServiceInterfaceAllocationPlan::NoNewServiceInterfaces)
    } else {
        Ok(ServiceInterfaceAllocationPlan::Allocate {
            intents,
            device_to_id,
            primary_dpu,
        })
    }
}

/// Creates each planned network segment, network prefix, Instance address, and service-interface
/// entry after `allocate_for_instance` locks `instance_addresses` and candidate VPC-prefix rows and,
/// for an existing Instance, revalidates the Instance and Machine.
async fn allocate_service_interfaces(
    txn: &mut db::Transaction<'_>,
    instance_id: InstanceId,
    current_network: &InstanceNetworkConfig,
    intents: Vec<EndpointIntent>,
    assignments: Vec<u32>,
    prefix_candidates: &BTreeMap<(VpcId, u8), Vec<VpcPrefixId>>,
    locked_prefixes: &mut HashMap<VpcPrefixId, VpcPrefix>,
) -> CarbideResult<InstanceNetworkConfig> {
    let mut network = current_network.clone();
    for (intent, slot_index) in intents.into_iter().zip(assignments) {
        let candidates = service_prefix_candidates(prefix_candidates, intent.vpc_id, intent.family)
            .ok_or_else(|| {
                CarbideError::internal(format!(
                    "validated service VPC {} lost its prefix candidates",
                    intent.vpc_id
                ))
            })?;
        let mut allocated = None;
        for candidate_id in candidates {
            let Some(candidate) = locked_prefixes.get_mut(candidate_id) else {
                continue;
            };
            if candidate.vpc_id != intent.vpc_id
                || service_family_key(intent.family)
                    != if candidate.config.prefix.is_ipv4() {
                        4
                    } else {
                        6
                    }
            {
                return Err(CarbideError::FailedPrecondition(format!(
                    "VPC prefix {} changed service-VPC allocation group",
                    candidate.id
                )));
            }
            for attempt in 0..super::PREFIX_ALLOCATION_TOTAL_ATTEMPTS {
                let mut savepoint = db::Transaction::begin_inner(txn.as_mut()).await?;
                let allocation = allocate_service_candidate_once(
                    savepoint.as_pgconn(),
                    instance_id,
                    intent,
                    slot_index,
                    candidate,
                )
                .await;

                match allocation {
                    Ok(Some(ServiceCandidateAllocation {
                        interface,
                        link_prefix,
                    })) => {
                        // Release the savepoint so its network segment, network prefix, address, and
                        // `last_used_prefix` update remain in the caller's activation transaction.
                        savepoint.commit().await?;
                        candidate.status.last_used_prefix = Some(link_prefix);
                        allocated = Some(interface);
                        break;
                    }
                    Ok(None) => {
                        savepoint.rollback().await?;
                        break;
                    }
                    Err(error) => {
                        let overlap_conflict = super::is_network_prefix_overlap_conflict(&error);
                        savepoint.rollback().await?;
                        // Retry only prefix-overlap conflicts; every other SQL failure aborts the
                        // outer transaction.
                        if !overlap_conflict {
                            return Err(error);
                        }
                        if attempt + 1 == super::PREFIX_ALLOCATION_TOTAL_ATTEMPTS {
                            return Err(CarbideError::UnavailableError(format!(
                                "allocation from service VPC {} prefix {} repeatedly raced with an overlapping network-prefix write; retry the request",
                                intent.vpc_id, candidate.id
                            )));
                        }
                    }
                }
            }
            if allocated.is_some() {
                break;
            }
        }
        network.service_interfaces.push(allocated.ok_or_else(|| {
            CarbideError::ResourceExhausted(format!(
                "service VPC {} has no available {:?} link prefix",
                intent.vpc_id, intent.family
            ))
        })?);
    }
    Ok(network)
}

/// Validates retained service-interface addresses and any requested host-network replacement when
/// no new service interfaces need to be allocated.
async fn validate_update_without_new_service_interfaces(
    txn: &mut db::Transaction<'_>,
    instance_id: InstanceId,
    current_network: &InstanceNetworkConfig,
    host: &ManagedHostStateSnapshot,
    active_endpoint_dpus: &BTreeSet<DpuMachineId>,
    host_network_update_requested: bool,
) -> CarbideResult<()> {
    // A host replacement preserving service interfaces locks `instance_addresses` before address
    // validation so VPC deletion cannot commit between VPC resolution and the address check.
    if host_network_update_requested && !current_network.service_interfaces.is_empty() {
        db::instance_address::lock_table_for_allocation(txn.as_mut()).await?;
    }

    // Active service interfaces require the replacement host VPC to remain FNN; service interfaces
    // referenced only by removed attachments still validate their persisted addresses during cleanup.
    if host_network_update_requested && !active_endpoint_dpus.is_empty() {
        let (_, device_to_id) = host.host_snapshot.get_dpu_device_and_id_mappings()?;
        let resolved_network = resolve_candidate_host_vpcs(txn, current_network).await?;
        let host_vpc_ids = host_vpc_ids_for_target_dpus(
            &resolved_network,
            &device_to_id,
            host.host_snapshot.primary_attached_dpu_machine_id(),
            active_endpoint_dpus,
        )?;
        validate_active_fnn_vpcs(txn, host_vpc_ids).await?;
    }

    validate_persisted_endpoint_addresses(txn.as_mut(), instance_id, current_network).await
}

/// Resolves candidate host-interface `vpc_id` values after `instance_addresses` is locked so overlap
/// and FNN validation use the requested `InstanceNetworkConfig`.
async fn resolve_candidate_host_vpcs(
    txn: &mut db::Transaction<'_>,
    current_network: &InstanceNetworkConfig,
) -> CarbideResult<InstanceNetworkConfig> {
    let mut resolved_network = current_network.clone();
    let explicit_prefix_ids = resolved_network
        .interfaces
        .iter()
        .filter(|interface| interface.vpc_id.is_none() && interface.vpc_selection.is_none())
        .filter_map(|interface| match interface.network_details.as_ref() {
            Some(NetworkDetails::VpcPrefixId(prefix_id)) => Some(*prefix_id),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let explicit_prefix_vpcs =
        db::vpc_prefix::get_for_allocation_by_ids(txn.as_mut(), &explicit_prefix_ids)
            .await?
            .into_iter()
            .map(|prefix| (prefix.id, prefix.vpc_id))
            .collect::<HashMap<_, _>>();
    apply_explicit_prefix_vpcs_for_service_planning(&mut resolved_network, &explicit_prefix_vpcs)?;
    for interface in &mut resolved_network.interfaces {
        if interface.vpc_id.is_none()
            && let Some(segment_id) = interface.network_segment_id
        {
            interface.vpc_id = db::vpc::find_by_segment(&mut *txn, segment_id)
                .await?
                .map(|vpc| vpc.id);
        }
    }
    Ok(resolved_network)
}

/// Collects every physical host-interface `vpc_id` for target DPUs and rejects targets with no
/// resolved VPC so FNN admission covers every host VPC without restricting a DPU to one VPC.
fn host_vpc_ids_for_target_dpus(
    network: &InstanceNetworkConfig,
    device_to_id: &HashMap<String, Vec<DpuMachineId>>,
    primary_dpu: Option<DpuMachineId>,
    target_dpus: &BTreeSet<DpuMachineId>,
) -> CarbideResult<BTreeSet<VpcId>> {
    let mut resolved_target_dpus = BTreeSet::new();
    let mut host_vpc_ids = BTreeSet::new();
    for interface in network
        .interfaces
        .iter()
        .filter(|interface| interface.function_id == InterfaceFunctionId::Physical {})
    {
        // Only locator-free legacy interfaces may fall back to the primary attached DPU.
        let dpu_id = match &interface.device_locator {
            Some(locator) => device_to_id
                .get(&locator.device)
                .and_then(|dpus| dpus.get(locator.device_instance))
                .copied()
                .ok_or_else(|| {
                    CarbideError::FailedPrecondition(format!(
                        "physical interface device {} instance {} does not resolve to an attached DPU",
                        locator.device, locator.device_instance
                    ))
                })?,
            None => primary_dpu.ok_or_else(|| {
                CarbideError::FailedPrecondition(
                    "a legacy physical interface cannot resolve a primary attached DPU".to_string(),
                )
            })?,
        };
        if !target_dpus.contains(&dpu_id) {
            continue;
        }
        let Some(vpc_id) = interface.vpc_id else {
            continue;
        };
        resolved_target_dpus.insert(dpu_id);
        host_vpc_ids.insert(vpc_id);
    }
    for dpu_id in target_dpus {
        if !resolved_target_dpus.contains(dpu_id) {
            return Err(CarbideError::FailedPrecondition(format!(
                "target DPU {dpu_id} has no resolved tenant-network VPC"
            )));
        }
    }
    Ok(host_vpc_ids)
}

/// Revalidates participating VPCs as live FNN networks while the address-table lock serializes VPC
/// deletion so admitted service connectivity cannot reference a concurrently deleted VPC.
async fn validate_active_fnn_vpcs(
    txn: &mut db::Transaction<'_>,
    vpc_ids: BTreeSet<VpcId>,
) -> CarbideResult<()> {
    for vpc_id in vpc_ids {
        let vpc = db::vpc::find_by_id_including_deleted(&mut *txn, vpc_id)
            .await?
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "vpc",
                id: vpc_id.to_string(),
            })?;
        if vpc.deleted.is_some() {
            return Err(CarbideError::FailedPrecondition(format!(
                "VPC {vpc_id} is deleted"
            )));
        }
        if vpc.config.network_virtualization_type != VpcVirtualizationType::Fnn {
            return Err(CarbideError::FailedPrecondition(format!(
                "VPC {vpc_id} must use FNN virtualization for a service attachment"
            )));
        }
    }
    Ok(())
}

/// Applies explicit VPC-prefix-to-VPC mappings before service planning so physical interfaces can
/// supply their host VPC when RPC conversion has removed the server-derived `vpc_id`.
fn apply_explicit_prefix_vpcs_for_service_planning(
    network: &mut InstanceNetworkConfig,
    prefix_vpcs: &HashMap<VpcPrefixId, VpcId>,
) -> CarbideResult<()> {
    for interface in &mut network.interfaces {
        if interface.vpc_id.is_some() {
            continue;
        }
        interface.vpc_id = interface.vpc_selection.map(|selection| selection.vpc_id);
        if interface.vpc_id.is_none()
            && let Some(NetworkDetails::VpcPrefixId(prefix_id)) = interface.network_details.as_ref()
        {
            interface.vpc_id = Some(*prefix_vpcs.get(prefix_id).ok_or_else(|| {
                CarbideError::FailedPrecondition(format!("VPC prefix `{prefix_id}` does not exist"))
            })?);
        }
    }
    Ok(())
}

/// Attempts one service endpoint allocation inside the caller's savepoint so a recognized prefix
/// exclusion race rolls back the generated segment, address, and cursor before retrying.
async fn allocate_service_candidate_once(
    txn: &mut sqlx::PgConnection,
    instance_id: InstanceId,
    intent: EndpointIntent,
    slot_index: u32,
    candidate: &model::vpc_prefix::VpcPrefix,
) -> CarbideResult<Option<ServiceCandidateAllocation>> {
    #[cfg(test)]
    if let Some(fault) = test_support::take_service_candidate_fault() {
        test_support::inject_service_candidate_fault(txn, fault).await?;
    }

    // Select the next canonical `link_prefix` while the VPC-prefix row lock protects
    // `last_used_prefix`.
    let link_prefix_len = match intent.family {
        ServiceVpcAddressFamily::Ipv4 => 31,
        ServiceVpcAddressFamily::Ipv6 => 127,
    };
    let allocator = PrefixAllocator::new(
        candidate.id,
        candidate.config.prefix,
        candidate.status.last_used_prefix,
        link_prefix_len,
    )?;
    let link_prefix = match allocator.next_free_prefix(&mut *txn).await {
        Ok(prefix) => prefix,
        Err(CarbideError::ResourceExhausted(_)) => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };

    // Persist the network segment, network prefix, address, and VPC prefix's `last_used_prefix`
    // together inside the savepoint.
    let (network_segment_id, network_prefix) = allocator
        .allocate_service_network_segment_for_prefix(&mut *txn, intent.vpc_id, link_prefix)
        .await?;
    db::vpc_prefix::update_last_used_prefix(&mut *txn, &candidate.id, link_prefix).await?;
    db::instance_address::insert_service_address(
        &mut *txn,
        instance_id,
        network_segment_id,
        intent.vpc_id,
        &network_prefix,
    )
    .await?;

    Ok(Some(ServiceCandidateAllocation {
        interface: InstanceServiceInterfaceConfig {
            attachment_id: intent.attachment_id,
            interface_ordinal: intent.interface_ordinal,
            dpu_id: intent.dpu_id,
            slot_index,
            vpc_id: intent.vpc_id,
            vpc_prefix_id: candidate.id,
            network_segment_id,
            network_prefix_id: network_prefix.id,
            link_prefix,
            mac_address: intent.mac_address,
            internal_uuid: uuid::Uuid::new_v4(),
        },
        link_prefix,
    }))
}

/// Verifies every persisted service interface against its registration, segment, prefix, and address
/// records so retries reject incomplete or inconsistent service-VPC activation.
#[allow(clippy::too_many_arguments)]
async fn validate_persisted_endpoints(
    txn: &mut sqlx::PgConnection,
    extension_services: &InstanceExtensionServicesConfig,
    network: &InstanceNetworkConfig,
    services: &HashMap<ExtensionServiceId, ExtensionService>,
    slot_count: u32,
    max_endpoints_per_dpu: u32,
) -> CarbideResult<()> {
    if network.service_interfaces.is_empty() {
        return Ok(());
    }

    // Validate each service interface's attachment, DPU, ordinal, UUID, and MAC fields before
    // batch-loading its VPC prefix, network segment, and network prefix.
    let mut internal_ids = HashSet::new();
    let mut logical_ids = HashSet::new();
    let mut segment_ids = HashSet::new();
    let mut prefix_ids = HashSet::new();
    let mut parent_ids = HashSet::new();
    let mut macs_by_service = HashMap::new();
    for endpoint in &network.service_interfaces {
        let invalid = |reason: &str| {
            CarbideError::FailedPrecondition(format!(
                "stored service interface {} is inconsistent: {reason}",
                endpoint.internal_uuid
            ))
        };
        if !internal_ids.insert(endpoint.internal_uuid) {
            return Err(invalid("internal identity is duplicated"));
        }
        if !logical_ids.insert((
            endpoint.attachment_id,
            endpoint.dpu_id,
            endpoint.interface_ordinal,
        )) {
            return Err(invalid(
                "attachment, DPU, and ordinal identity is duplicated",
            ));
        }
        if !segment_ids.insert(endpoint.network_segment_id) {
            return Err(invalid(
                "generated network segment is shared by another endpoint",
            ));
        }
        if !prefix_ids.insert(endpoint.network_prefix_id) {
            return Err(invalid(
                "generated network prefix is shared by another endpoint",
            ));
        }
        parent_ids.insert(endpoint.vpc_prefix_id);

        let mut owners = extension_services
            .service_configs
            .iter()
            .filter(|attachment| attachment.id == Some(endpoint.attachment_id));
        let owner = owners
            .next()
            .ok_or_else(|| invalid("attachment owner is missing"))?;
        if owners.next().is_some() {
            return Err(invalid("attachment owner is duplicated"));
        }
        let service = services
            .get(&owner.service_id)
            .ok_or_else(|| invalid("owning extension service is missing"))?;
        let requirement = usize::try_from(endpoint.interface_ordinal)
            .ok()
            .and_then(|ordinal| service.service_vpc_interfaces.get(ordinal))
            .ok_or_else(|| invalid("interface ordinal is not registered by its service"))?;
        endpoint
            .validate_for_family(requirement.address_family)
            .map_err(CarbideError::from)?;

        let macs = match macs_by_service.entry(service.id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                db::extension_service::find_interface_macs(&mut *txn, service.id)
                    .await?
                    .into_iter()
                    .map(|assignment| (assignment.interface_ordinal, assignment.mac_address))
                    .collect::<HashMap<_, _>>(),
            ),
        };
        if macs.get(&endpoint.interface_ordinal) != Some(&endpoint.mac_address) {
            return Err(invalid(
                "MAC address does not match its durable service assignment",
            ));
        }
    }
    assign_slots(
        &network.service_interfaces,
        &[],
        slot_count,
        max_endpoints_per_dpu,
    )?;

    let parent_ids = parent_ids.into_iter().collect::<Vec<_>>();
    let parents = db::vpc_prefix::get_for_allocation_by_ids(&mut *txn, &parent_ids)
        .await?
        .into_iter()
        .map(|prefix| (prefix.id, prefix))
        .collect::<HashMap<_, _>>();
    let segment_ids = segment_ids.into_iter().collect::<Vec<_>>();
    let segments = db::network_segment::find_by(
        &mut *txn,
        ObjectColumnFilter::List(db::network_segment::IdColumn, &segment_ids),
        NetworkSegmentSearchConfig::default(),
    )
    .await?
    .into_iter()
    .map(|segment| (segment.id, segment))
    .collect::<HashMap<_, _>>();
    let expected_mtu = i32::try_from(carbide_dpf::SERVICE_VPC_MTU).map_err(|error| {
        CarbideError::internal(format!(
            "service-VPC MTU {} cannot be compared with network segments: {error}",
            carbide_dpf::SERVICE_VPC_MTU
        ))
    })?;

    // Validate the network-segment and network-prefix fields used by service connectivity and cleanup.
    for endpoint in &network.service_interfaces {
        let invalid = |reason: &str| {
            CarbideError::FailedPrecondition(format!(
                "stored service interface {} is inconsistent: {reason}",
                endpoint.internal_uuid
            ))
        };
        let parent = parents
            .get(&endpoint.vpc_prefix_id)
            .ok_or_else(|| invalid("VPC prefix is missing"))?;
        if parent.vpc_id != endpoint.vpc_id
            || parent.config.prefix.is_ipv4() != endpoint.link_prefix.is_ipv4()
            || !parent
                .config
                .prefix
                .contains(endpoint.link_prefix.network())
            || !parent
                .config
                .prefix
                .contains(endpoint.link_prefix.broadcast())
        {
            return Err(invalid(
                "link prefix is not contained by its owning VPC prefix",
            ));
        }

        let segment = segments
            .get(&endpoint.network_segment_id)
            .filter(|segment| !segment.is_marked_as_deleted())
            .ok_or_else(|| invalid("generated network segment is missing or deleted"))?;
        if segment.config.vpc_id != Some(endpoint.vpc_id)
            || segment.config.mtu != expected_mtu
            || segment.config.segment_type != NetworkSegmentType::Tenant
            || segment.status.can_stretch != Some(false)
            || segment.config.allocation_strategy != AllocationStrategy::Dynamic
            || segment.config.infer_slaac_eui64_addresses
        {
            return Err(invalid("generated network segment contract does not match"));
        }
        let [prefix] = segment.prefixes.as_slice() else {
            return Err(invalid(
                "generated network segment does not have exactly one prefix",
            ));
        };
        let expected_gateway = endpoint
            .link_prefix
            .is_ipv4()
            .then_some(endpoint.link_prefix.network());
        let expected_reserved = if endpoint.link_prefix.is_ipv6() { 1 } else { 0 };
        if prefix.id != endpoint.network_prefix_id
            || prefix.segment_id != endpoint.network_segment_id
            || prefix.prefix != endpoint.link_prefix
            || prefix.vpc_prefix_id != Some(endpoint.vpc_prefix_id)
            || prefix.vpc_prefix != Some(parent.config.prefix)
            || prefix.gateway != expected_gateway
            || prefix.dhcpv6_link_address.is_some()
            || prefix.num_reserved != expected_reserved
            || prefix.svi_ip.is_some()
        {
            return Err(invalid("generated network prefix contract does not match"));
        }
    }
    Ok(())
}

/// Verifies each retained endpoint's normalized address after any required table lock is held so a
/// host-network update cannot deadlock while upgrading an earlier address-table read lock.
async fn validate_persisted_endpoint_addresses(
    txn: &mut sqlx::PgConnection,
    instance_id: InstanceId,
    network: &InstanceNetworkConfig,
) -> CarbideResult<()> {
    if network.service_interfaces.is_empty() {
        return Ok(());
    }

    let segment_ids = network
        .service_interfaces
        .iter()
        .map(|endpoint| endpoint.network_segment_id)
        .collect::<Vec<_>>();
    let addresses = db::instance_address::find_all_by_segment_ids(&mut *txn, &segment_ids).await?;
    for endpoint in &network.service_interfaces {
        let invalid = |reason: &str| {
            CarbideError::FailedPrecondition(format!(
                "stored service interface {} is inconsistent: {reason}",
                endpoint.internal_uuid
            ))
        };
        let expected_address =
            db::instance_address::service_address_for_prefix(endpoint.link_prefix)?;
        let endpoint_addresses = addresses
            .iter()
            .filter(|address| address.segment_id == endpoint.network_segment_id)
            .collect::<Vec<_>>();
        let [address] = endpoint_addresses.as_slice() else {
            return Err(invalid(
                "generated network segment does not have exactly one normalized address",
            ));
        };
        if address.instance_id != instance_id
            || address.vpc_id != endpoint.vpc_id
            || address.address != expected_address
            || address.prefix != endpoint.link_prefix
            || address.hostname.is_some()
        {
            return Err(invalid(
                "normalized service address contract does not match",
            ));
        }
    }
    Ok(())
}

/// Validates a networked registration and its exact caller VPC vector before any allocation can
/// reserve slots, prefixes, or normalized addresses.
fn validate_service_registration(
    service: &ExtensionService,
    selected_vpcs: &[VpcId],
    new_attachment: bool,
) -> CarbideResult<()> {
    if service.service_type != ExtensionServiceType::DpfHelmChart {
        return Err(CarbideError::FailedPrecondition(format!(
            // xtask:allow-error-case: DPF Helm is a registered service type name
            "extension service {} is not a DPF Helm registration",
            service.id
        )));
    }
    if selected_vpcs.len() != service.service_vpc_interfaces.len() {
        return Err(CarbideError::InvalidArgument(format!(
            "extension service {} requires {} service VPC selections but received {}",
            service.id,
            service.service_vpc_interfaces.len(),
            selected_vpcs.len()
        )));
    }
    if new_attachment
        && (service.deleted.is_some()
            || service.status.controller_state.value != ExtensionServiceLifecycleState::Ready)
    {
        return Err(CarbideError::FailedPrecondition(format!(
            // xtask:allow-error-case: DPF Helm is a registered service type name
            "extension service {} does not have a converged DPF Helm registration",
            service.id
        )));
    }
    if new_attachment {
        validate_implementation_support(service)?;
    }
    Ok(())
}

/// Rejects a new service binding that overlaps either retained or requested host networking so a
/// host-interface removal cannot authorize service activation before that removal converges.
fn validate_new_attachment_overlap(
    allow_overlap: bool,
    requested_network: &InstanceNetworkConfig,
    retained_network: Option<&InstanceNetworkConfig>,
    intents: &[EndpointIntent],
) -> CarbideResult<()> {
    if allow_overlap {
        return Ok(());
    }
    let host_vpcs = retained_network
        .into_iter()
        .chain(std::iter::once(requested_network))
        .flat_map(|network| &network.interfaces)
        .filter_map(|interface| interface.vpc_id)
        .collect::<HashSet<_>>();
    if let Some(intent) = intents
        .iter()
        .find(|intent| host_vpcs.contains(&intent.vpc_id))
    {
        return Err(CarbideError::FailedPrecondition(format!(
            "VPC {} cannot be used by both a tenant interface and service attachment {}",
            intent.vpc_id, intent.attachment_id
        )));
    }
    Ok(())
}

/// Converts a service family to a compact stable map key used only for exact prefix grouping.
const fn service_family_key(family: ServiceVpcAddressFamily) -> u8 {
    match family {
        ServiceVpcAddressFamily::Ipv4 => 4,
        ServiceVpcAddressFamily::Ipv6 => 6,
    }
}

/// Returns only prefix candidates registered for the requested VPC and address family so
/// exhaustion never redirects an endpoint to the other family.
fn service_prefix_candidates(
    candidates: &BTreeMap<(VpcId, u8), Vec<VpcPrefixId>>,
    vpc_id: VpcId,
    family: ServiceVpcAddressFamily,
) -> Option<&[VpcPrefixId]> {
    candidates
        .get(&(vpc_id, service_family_key(family)))
        .map(Vec::as_slice)
        .filter(|candidates| !candidates.is_empty())
}

/// Assigns one DPU-local fixed slot to each new `(DPU, VPC)` request, reuses a VPC's existing slot,
/// and reserves every persisted service-interface slot until reclamation.
pub(crate) fn assign_slots(
    existing: &[InstanceServiceInterfaceConfig],
    requested: &[(DpuMachineId, VpcId)],
    slot_count: u32,
    max_endpoints_per_dpu: u32,
) -> CarbideResult<Vec<u32>> {
    let mut vpc_slots = BTreeMap::<DpuMachineId, BTreeMap<VpcId, u32>>::new();
    let mut slot_vpcs = BTreeMap::<DpuMachineId, BTreeMap<u32, VpcId>>::new();
    let mut endpoint_counts = BTreeMap::<DpuMachineId, u32>::new();

    // Reconstruct VPC-to-slot and slot-to-VPC assignments because duplicates in either map make
    // retries unsafe.
    for endpoint in existing {
        let previous_slot = vpc_slots
            .entry(endpoint.dpu_id)
            .or_default()
            .insert(endpoint.vpc_id, endpoint.slot_index);
        let previous_vpc = slot_vpcs
            .entry(endpoint.dpu_id)
            .or_default()
            .insert(endpoint.slot_index, endpoint.vpc_id);
        if previous_slot.is_some_and(|slot| slot != endpoint.slot_index)
            || previous_vpc.is_some_and(|vpc| vpc != endpoint.vpc_id)
        {
            return Err(CarbideError::FailedPrecondition(format!(
                "stored service-VPC slot assignments are inconsistent on DPU {}",
                endpoint.dpu_id
            )));
        }
        let count = endpoint_counts.entry(endpoint.dpu_id).or_default();
        *count = count.checked_add(1).ok_or_else(|| {
            CarbideError::internal(format!(
                "stored service-VPC endpoint count overflowed for DPU {}",
                endpoint.dpu_id
            ))
        })?;
    }

    let mut additional_counts = BTreeMap::<DpuMachineId, u32>::new();
    for (dpu_id, _) in requested {
        let count = additional_counts.entry(*dpu_id).or_default();
        *count = count.checked_add(1).ok_or_else(|| {
            CarbideError::internal(format!(
                "requested service-VPC endpoint count overflowed for DPU {dpu_id}"
            ))
        })?;
    }
    for (dpu_id, additional) in additional_counts {
        let occupied = endpoint_counts.get(&dpu_id).copied().unwrap_or_default();
        if u64::from(occupied) + u64::from(additional) > u64::from(max_endpoints_per_dpu) {
            return Err(CarbideError::ResourceExhausted(format!(
                "DPU {dpu_id} has no service-VPC endpoint capacity ({occupied} retained, {additional} requested, {max_endpoints_per_dpu} maximum)"
            )));
        }
    }

    let mut assignments = Vec::with_capacity(requested.len());
    for (dpu_id, vpc_id) in requested {
        if let Some(slot) = vpc_slots
            .get(dpu_id)
            .and_then(|assignments| assignments.get(vpc_id))
            .copied()
        {
            if slot >= slot_count {
                return Err(CarbideError::ResourceExhausted(format!(
                    "DPU {dpu_id} retains VPC {vpc_id} in service-VPC slot {slot}, outside the configured {slot_count} slots"
                )));
            }
            assignments.push(slot);
            continue;
        }

        let used = slot_vpcs.entry(*dpu_id).or_default();
        let slot = (0..slot_count)
            .find(|slot| !used.contains_key(slot))
            .ok_or_else(|| {
                CarbideError::ResourceExhausted(format!(
                    "DPU {dpu_id} has no free service-VPC slot for VPC {vpc_id}"
                ))
            })?;
        used.insert(slot, *vpc_id);
        vpc_slots.entry(*dpu_id).or_default().insert(*vpc_id, slot);
        assignments.push(slot);
    }

    Ok(assignments)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct HostVpcBinding {
    function_id: InterfaceFunctionId,
    device_locator: Option<DeviceLocator>,
    vpc_id: VpcId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ServiceVpcBinding {
    attachment_id: uuid::Uuid,
    dpu_id: DpuMachineId,
    interface_ordinal: u32,
    vpc_id: VpcId,
}

/// Rejects only newly introduced tenant-host and service-interface VPC conflicts, preserving exact
/// bindings accepted under an earlier permissive configuration during retries and removals.
pub(crate) fn validate_tenant_overlap(
    allow_overlap: bool,
    baseline_networks: &[&InstanceNetworkConfig],
    candidate_networks: &[&InstanceNetworkConfig],
) -> CarbideResult<()> {
    if allow_overlap {
        return Ok(());
    }

    let baseline_conflicts = overlap_conflicts(baseline_networks);
    let candidate_conflicts = overlap_conflicts(candidate_networks);
    if let Some((host, service)) = candidate_conflicts.difference(&baseline_conflicts).next() {
        return Err(CarbideError::FailedPrecondition(format!(
            "VPC {} cannot be used by both tenant interface {:?} and service attachment {} on DPU {}",
            host.vpc_id, host.function_id, service.attachment_id, service.dpu_id
        )));
    }
    Ok(())
}

/// Collects exact conflicting host and service bindings so grandfathering cannot authorize a
/// different interface merely because it uses a previously overlapping VPC.
fn overlap_conflicts(
    networks: &[&InstanceNetworkConfig],
) -> HashSet<(HostVpcBinding, ServiceVpcBinding)> {
    let hosts = networks
        .iter()
        .flat_map(|network| &network.interfaces)
        .filter_map(|interface| {
            Some(HostVpcBinding {
                function_id: interface.function_id.clone(),
                device_locator: interface.device_locator.clone(),
                vpc_id: interface.vpc_id?,
            })
        })
        .collect::<HashSet<_>>();
    let services = networks
        .iter()
        .flat_map(|network| &network.service_interfaces)
        .map(|interface| ServiceVpcBinding {
            attachment_id: interface.attachment_id,
            dpu_id: interface.dpu_id,
            interface_ordinal: interface.interface_ordinal,
            vpc_id: interface.vpc_id,
        })
        .collect::<HashSet<_>>();

    hosts
        .into_iter()
        .flat_map(|host| {
            services
                .iter()
                .filter(move |service| service.vpc_id == host.vpc_id)
                .map(move |service| (host.clone(), *service))
        })
        .collect()
}

#[cfg(test)]
pub(crate) use test_support::{
    ServiceCandidateFault, allocate_for_instance_with_supported_families_for_test,
    with_service_candidate_faults_for_test, with_service_vpc_implementation_support_for_test,
};

#[cfg(test)]
mod test_support {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        Api, CarbideResult, InstanceExtensionServicesConfig, InstanceId, InstanceNetworkConfig,
        InstanceSnapshot, ManagedHostStateSnapshot, RequestedInstanceExtensionServicesConfig,
        allocate_for_instance,
    };

    /// Selects one PostgreSQL failure injected into service-prefix allocation so transaction tests
    /// exercise production classification and savepoint rollback without a mock database implementation.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum ServiceCandidateFault {
        PrefixOverlap,
        SerializationFailure,
        Deadlock,
        UnrelatedDatabaseFailure,
    }

    /// Holds one task's ordered service-candidate failures and attempt counter so concurrent allocator
    /// tests cannot consume or observe each other's injected PostgreSQL faults.
    struct ServiceCandidateFaultPlan {
        faults: std::cell::RefCell<VecDeque<ServiceCandidateFault>>,
        attempts: Arc<AtomicUsize>,
    }

    tokio::task_local! {
        static SERVICE_CANDIDATE_FAULTS: ServiceCandidateFaultPlan;
    }

    tokio::task_local! {
        static SERVICE_VPC_IMPLEMENTATION_SUPPORTED_FOR_TEST: ();
    }

    /// Returns whether this test task opened the implementation gate so every other task retains
    /// production's fail-closed behavior.
    pub(super) fn implementation_support_enabled() -> bool {
        SERVICE_VPC_IMPLEMENTATION_SUPPORTED_FOR_TEST
            .try_with(|_| ())
            .is_ok()
    }

    /// Consumes one configured fault and counts the allocation attempt so concurrent tests cannot
    /// share fault-injection state.
    pub(super) fn take_service_candidate_fault() -> Option<ServiceCandidateFault> {
        SERVICE_CANDIDATE_FAULTS
            .try_with(|plan| {
                plan.attempts.fetch_add(1, Ordering::Relaxed);
                plan.faults.borrow_mut().pop_front()
            })
            .ok()
            .flatten()
    }

    /// Runs one test future with the implementation gate open so public API tests can exercise
    /// atomic activation without changing the production family-support result.
    pub(crate) async fn with_service_vpc_implementation_support_for_test<F>(future: F) -> F::Output
    where
        F: Future,
    {
        SERVICE_VPC_IMPLEMENTATION_SUPPORTED_FOR_TEST
            .scope((), future)
            .await
    }

    /// Runs the production coordinator with the task-local implementation gate open so tests can
    /// prove its transaction and locking behavior before the production family gate is enabled.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn allocate_for_instance_with_supported_families_for_test(
        api: &Api,
        txn: &mut db::Transaction<'_>,
        instance_id: InstanceId,
        extension_services: &InstanceExtensionServicesConfig,
        requested: &RequestedInstanceExtensionServicesConfig,
        current_network: &InstanceNetworkConfig,
        host: &ManagedHostStateSnapshot,
        expected_machine_has_cx9: bool,
        expected_instance: Option<&InstanceSnapshot>,
    ) -> CarbideResult<InstanceNetworkConfig> {
        with_service_vpc_implementation_support_for_test(allocate_for_instance(
            api,
            txn,
            instance_id,
            extension_services,
            requested,
            current_network,
            host,
            expected_machine_has_cx9,
            expected_instance,
        ))
        .await
    }

    /// Runs service-prefix allocation with task-local PostgreSQL faults and returns the number of
    /// real allocation attempts so retry tests can distinguish recovery from an early rejection.
    pub(crate) async fn with_service_candidate_faults_for_test<F>(
        faults: Vec<ServiceCandidateFault>,
        future: F,
    ) -> (F::Output, usize)
    where
        F: Future,
    {
        let attempts = Arc::new(AtomicUsize::new(0));
        let plan = ServiceCandidateFaultPlan {
            faults: std::cell::RefCell::new(faults.into()),
            attempts: Arc::clone(&attempts),
        };
        let output = SERVICE_CANDIDATE_FAULTS.scope(plan, future).await;
        (output, attempts.load(Ordering::Relaxed))
    }

    /// Raises a real PostgreSQL error inside the allocation savepoint so tests cover the same
    /// SQLSTATE and constraint metadata that production classification consumes.
    pub(super) async fn inject_service_candidate_fault(
        txn: &mut sqlx::PgConnection,
        fault: ServiceCandidateFault,
    ) -> CarbideResult<()> {
        let query = match fault {
            ServiceCandidateFault::PrefixOverlap => {
                "DO $service_vpc_fault$ BEGIN RAISE EXCEPTION 'injected service-prefix overlap' USING ERRCODE = '23P01', CONSTRAINT = 'network_prefixes_prefix_excl'; END $service_vpc_fault$"
            }
            ServiceCandidateFault::SerializationFailure => {
                "DO $service_vpc_fault$ BEGIN RAISE EXCEPTION 'injected serialization failure' USING ERRCODE = '40001'; END $service_vpc_fault$"
            }
            ServiceCandidateFault::Deadlock => {
                "DO $service_vpc_fault$ BEGIN RAISE EXCEPTION 'injected deadlock' USING ERRCODE = '40P01'; END $service_vpc_fault$"
            }
            ServiceCandidateFault::UnrelatedDatabaseFailure => "SELECT 1 / 0",
        };
        sqlx::query(query)
            .execute(&mut *txn)
            .await
            .map(|_| ())
            .map_err(|error| db::DatabaseError::query(query, error).into())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    use carbide_uuid::machine::DpuMachineId;
    use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
    use carbide_uuid::vpc::{VpcId, VpcPrefixId};
    use chrono::Utc;
    use config_version::ConfigVersion;
    use ipnetwork::IpNetwork;
    use mac_address::MacAddress;
    use model::extension_service::ServiceVpcAddressFamily;
    use model::instance::config::extension_services::{
        InstanceExtensionServiceConfig, InstanceExtensionServicesConfig,
    };
    use model::instance::config::network::{
        DeviceLocator, InstanceNetworkConfig, InstanceServiceInterfaceConfig, InterfaceFunctionId,
        NetworkDetails,
    };

    use super::{
        EndpointIntent, apply_explicit_prefix_vpcs_for_service_planning, assign_slots,
        host_vpc_ids_for_target_dpus, service_prefix_candidates, terminate_networked_attachments,
        validate_new_attachment_overlap, validate_tenant_overlap,
    };

    /// Verifies explicit-prefix physical interfaces recover their owning VPC before service
    /// planning because RPC conversion intentionally omits the server-derived `vpc_id`.
    #[test]
    fn explicit_prefix_vpc_supplies_host_vpc_for_service_planning() {
        // Remove every derived field so the explicit VPC prefix is the only source of VPC selection.
        let prefix_id = VpcPrefixId::new();
        let vpc_id = VpcId::new();
        let mut network = InstanceNetworkConfig::for_segment_ids(
            &[NetworkSegmentId::new()],
            &[],
            &[VpcId::new()],
        );
        let interface = &mut network.interfaces[0];
        interface.network_details = Some(NetworkDetails::VpcPrefixId(prefix_id));
        interface.network_segment_id = None;
        interface.vpc_selection = None;
        interface.vpc_id = None;

        // Applying the batch-loaded VPC-prefix map restores the VPC required by DPU planning.
        apply_explicit_prefix_vpcs_for_service_planning(
            &mut network,
            &HashMap::from([(prefix_id, vpc_id)]),
        )
        .expect("configured explicit prefix must resolve");
        assert_eq!(network.interfaces[0].vpc_id, Some(vpc_id));
    }

    /// Verifies physical interfaces on one DPU may use different VPCs because FNN admission must
    /// validate every host VPC without imposing a one-VPC-per-DPU restriction.
    #[test]
    fn host_vpc_resolution_allows_multiple_vpcs_per_dpu() {
        // Map two physical interface device names to the same DPU and assign each interface a
        // different VPC.
        let dpu_id = model::test_support::machine_snapshot::dpu_machine_id(0);
        let first_vpc_id = VpcId::new();
        let second_vpc_id = VpcId::new();
        let first_device = "first-device".to_string();
        let second_device = "second-device".to_string();
        let network = InstanceNetworkConfig::for_segment_ids(
            &[NetworkSegmentId::new(), NetworkSegmentId::new()],
            &[
                DeviceLocator {
                    device: first_device.clone(),
                    device_instance: 0,
                },
                DeviceLocator {
                    device: second_device.clone(),
                    device_instance: 0,
                },
            ],
            &[first_vpc_id, second_vpc_id],
        );
        let device_to_id =
            HashMap::from([(first_device, vec![dpu_id]), (second_device, vec![dpu_id])]);

        // Resolve both physical interfaces for the targeted DPU.
        let host_vpc_ids =
            host_vpc_ids_for_target_dpus(&network, &device_to_id, None, &BTreeSet::from([dpu_id]))
                .expect("one DPU may service multiple host VPCs");

        // The resolved set retains both VPC IDs instead of rejecting or dropping either interface.
        assert_eq!(host_vpc_ids, BTreeSet::from([first_vpc_id, second_vpc_id]));
    }

    /// Verifies family-specific candidate lookup never substitutes available capacity from the
    /// other address family because a registration's family is an immutable endpoint contract.
    #[test]
    fn prefix_candidates_do_not_fall_back_between_families() {
        // Give one VPC capacity in both families and a second VPC only IPv6 capacity.
        let complete_vpc = VpcId::new();
        let ipv6_only_vpc = VpcId::new();
        let complete_ipv4 = VpcPrefixId::new();
        let complete_ipv6 = VpcPrefixId::new();
        let ipv6_only = VpcPrefixId::new();
        let candidates = BTreeMap::from([
            ((complete_vpc, 4), vec![complete_ipv4]),
            ((complete_vpc, 6), vec![complete_ipv6]),
            ((ipv6_only_vpc, 6), vec![ipv6_only]),
        ]);

        // Exact family keys return their own capacity while absent IPv4 remains unavailable.
        assert_eq!(
            service_prefix_candidates(&candidates, complete_vpc, ServiceVpcAddressFamily::Ipv4,),
            Some([complete_ipv4].as_slice())
        );
        assert_eq!(
            service_prefix_candidates(&candidates, complete_vpc, ServiceVpcAddressFamily::Ipv6,),
            Some([complete_ipv6].as_slice())
        );
        assert_eq!(
            service_prefix_candidates(&candidates, ipv6_only_vpc, ServiceVpcAddressFamily::Ipv4,),
            None
        );
    }

    /// Builds a stored service interface with only slot-driving fields varied so slot tests remain
    /// focused on DPU-local VPC occupancy rather than other service-interface fields.
    fn endpoint(
        dpu_id: DpuMachineId,
        vpc_id: VpcId,
        slot_index: u32,
    ) -> InstanceServiceInterfaceConfig {
        InstanceServiceInterfaceConfig {
            attachment_id: uuid::Uuid::new_v4(),
            interface_ordinal: 0,
            dpu_id,
            slot_index,
            vpc_id,
            vpc_prefix_id: VpcPrefixId::new(),
            network_segment_id: NetworkSegmentId::new(),
            network_prefix_id: NetworkPrefixId::new(),
            link_prefix: "192.0.2.0/31"
                .parse::<IpNetwork>()
                .expect("valid link prefix"),
            mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
            internal_uuid: uuid::Uuid::new_v4(),
        }
    }

    /// Verifies same-VPC requests share one slot from either an empty or persisted service-interface
    /// set while each DPU chooses its own lowest free slot.
    #[test]
    fn slot_assignment_reuses_vpc_and_selects_lowest_free_per_dpu() {
        // Give the first DPU VPC A in slot one and the second DPU VPC B in slot zero.
        let first_dpu = model::test_support::machine_snapshot::dpu_machine_id(0);
        let second_dpu = model::test_support::machine_snapshot::dpu_machine_id(1);
        let vpc_a = VpcId::new();
        let vpc_b = VpcId::new();
        let vpc_c = VpcId::new();
        let existing = [
            endpoint(first_dpu, vpc_a, 1),
            endpoint(second_dpu, vpc_b, 0),
        ];

        // Request one shared VPC and one new VPC on each independently occupied DPU.
        let assignments = assign_slots(
            &existing,
            &[(first_dpu, vpc_a), (first_dpu, vpc_c), (second_dpu, vpc_c)],
            3,
            4,
        )
        .expect("available slots");

        // Existing VPC A keeps slot one while VPC C takes each DPU's lowest local free slot.
        assert_eq!(assignments, vec![1, 0, 1]);

        // Two endpoints for one new DPU/VPC pair must establish and reuse one slot in this batch.
        let shared_new_vpc = assign_slots(&[], &[(first_dpu, vpc_c), (first_dpu, vpc_c)], 3, 2)
            .expect("same-VPC batch shares an available slot");
        assert_eq!(shared_new_vpc, vec![0, 0]);
    }

    /// Verifies every retained or newly requested endpoint consumes endpoint capacity even when
    /// several endpoints share one VPC slot because each owns a service-facing SF reservation.
    #[test]
    fn slot_assignment_counts_every_retained_endpoint_against_capacity() {
        // A persisted service interface consumes the same endpoint capacity as an active attachment.
        let dpu_id = model::test_support::machine_snapshot::dpu_machine_id(0);
        let existing = [endpoint(dpu_id, VpcId::new(), 0)];

        // Attempt to add another endpoint when the single endpoint reservation is occupied.
        let error = assign_slots(&existing, &[(dpu_id, VpcId::new())], 2, 1)
            .expect_err("retained endpoint must consume capacity");

        // Capacity exhaustion is reported before any slot choice can authorize allocation.
        assert!(matches!(error, crate::CarbideError::ResourceExhausted(_)));

        // Slot sharing must not let two new endpoints bypass a one-endpoint DPU limit.
        let shared_vpc = VpcId::new();
        let error = assign_slots(&[], &[(dpu_id, shared_vpc), (dpu_id, shared_vpc)], 2, 1)
            .expect_err("each endpoint consumes capacity even when its VPC slot is shared");
        assert!(matches!(error, crate::CarbideError::ResourceExhausted(_)));
    }

    /// Verifies the retained DPU-local slot map is one-to-one because either two VPCs sharing a
    /// slot or one VPC occupying two slots makes retries and cleanup ambiguous.
    #[test]
    fn slot_assignment_rejects_conflicting_stored_mapping() {
        // Put two persisted VPCs in the same slot on one DPU to model conflicting stored assignments.
        let dpu_id = model::test_support::machine_snapshot::dpu_machine_id(0);
        let existing = [
            endpoint(dpu_id, VpcId::new(), 0),
            endpoint(dpu_id, VpcId::new(), 0),
        ];

        // Reconstruct the DPU slot map before assigning any additional endpoint.
        let error = assign_slots(&existing, &[], 2, 2)
            .expect_err("conflicting stored slot ownership must fail closed");

        // An inconsistent stored slot assignment is rejected rather than resolved by request order.
        assert!(matches!(error, crate::CarbideError::FailedPrecondition(_)));

        // One retained VPC in two slots is the inverse corruption and must also fail closed.
        let vpc_id = VpcId::new();
        let inverse = [endpoint(dpu_id, vpc_id, 0), endpoint(dpu_id, vpc_id, 1)];
        let error = assign_slots(&inverse, &[], 2, 2)
            .expect_err("one retained VPC cannot occupy two DPU-local slots");
        assert!(matches!(error, crate::CarbideError::FailedPrecondition(_)));
    }

    /// Verifies a reduced slot count cannot authorize another endpoint on a retained higher slot
    /// because registration keeps that slot only for drain, not for new activation.
    #[test]
    fn slot_assignment_rejects_new_use_of_draining_slot() {
        // Retain VPC A in slot three after the operator reduces the configured inventory to two.
        let dpu_id = model::test_support::machine_snapshot::dpu_machine_id(0);
        let vpc_id = VpcId::new();
        let existing = [endpoint(dpu_id, vpc_id, 3)];

        // Request another endpoint sharing VPC A while the higher slot is draining.
        let error = assign_slots(&existing, &[(dpu_id, vpc_id)], 2, 2)
            .expect_err("a draining slot cannot accept new endpoints");

        // Allocation reports unavailable capacity without changing the retained mapping.
        assert!(matches!(error, crate::CarbideError::ResourceExhausted(_)));
    }

    /// Verifies whole-instance termination fans one timestamp across active networked attachments
    /// and preserves an earlier removal timestamp because retries must not restart cleanup timing.
    #[test]
    fn whole_instance_termination_fans_out_and_preserves_existing_timestamps() {
        // Build two active networked attachments and one attachment already being removed.
        let service_id = carbide_uuid::extension_service::ExtensionServiceId::new();
        let first_active_id = uuid::Uuid::new_v4();
        let second_active_id = uuid::Uuid::new_v4();
        let terminating_id = uuid::Uuid::new_v4();
        let earlier_removal = Utc::now() - chrono::Duration::minutes(1);
        let current = InstanceExtensionServicesConfig {
            service_configs: vec![
                InstanceExtensionServiceConfig {
                    id: Some(first_active_id),
                    service_id,
                    version: ConfigVersion::initial(),
                    dpu_target: None,
                    removed: None, // Active at termination start.
                },
                InstanceExtensionServiceConfig {
                    id: Some(second_active_id),
                    service_id: carbide_uuid::extension_service::ExtensionServiceId::new(),
                    version: ConfigVersion::initial(),
                    dpu_target: None,
                    removed: None, // Active at termination start.
                },
                InstanceExtensionServiceConfig {
                    id: Some(terminating_id),
                    service_id: carbide_uuid::extension_service::ExtensionServiceId::new(),
                    version: ConfigVersion::initial(),
                    dpu_target: None,
                    removed: Some(earlier_removal), // Already draining.
                },
            ],
        };
        // Persisted service-interface attachment IDs, rather than vector position, drive termination.
        let mut first_active_endpoint = endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(0),
            VpcId::new(),
            0,
        );
        first_active_endpoint.attachment_id = first_active_id;
        let mut second_active_endpoint = endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(1),
            VpcId::new(),
            0,
        );
        second_active_endpoint.attachment_id = second_active_id;
        let mut terminating_endpoint = endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(0),
            VpcId::new(),
            1,
        );
        terminating_endpoint.attachment_id = terminating_id;
        let network = InstanceNetworkConfig {
            service_interfaces: vec![
                first_active_endpoint,
                second_active_endpoint,
                terminating_endpoint,
            ],
            ..Default::default()
        };
        let release_time = Utc::now();

        // Mark both active attachments while preserving the already-terminating timestamp.
        let updated = terminate_networked_attachments(&current, &network, release_time)
            .expect("the active attachment must change");

        // Both active attachments receive one release timestamp while the existing timestamp survives.
        assert_eq!(updated.service_configs[0].removed, Some(release_time));
        assert_eq!(updated.service_configs[1].removed, Some(release_time));
        assert_eq!(updated.service_configs[2].removed, Some(earlier_removal));
    }

    /// Verifies overlap grandfathering follows exact `HostVpcBinding` and `ServiceVpcBinding` values because a
    /// previously shared VPC must not authorize another service attachment after policy tightens.
    #[test]
    fn overlap_grandfathers_only_exact_existing_bindings() {
        // Build one accepted host/service conflict that predates the restrictive configuration.
        let vpc_id = VpcId::new();
        let segment_id = NetworkSegmentId::new();
        let locator = model::instance::config::network::DeviceLocator {
            device: "eth0".to_string(),
            device_instance: 0,
        };
        let mut baseline =
            InstanceNetworkConfig::for_segment_ids(&[segment_id], &[locator], &[vpc_id]);
        baseline.service_interfaces.push(endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(0),
            vpc_id,
            0,
        ));

        // Retry the exact binding, then add a distinct service binding in the same VPC.
        validate_tenant_overlap(false, &[&baseline], &[&baseline])
            .expect("exact grandfathered binding");
        let mut expanded = baseline.clone();
        expanded.service_interfaces.push(endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(1),
            vpc_id,
            0,
        ));
        let error = validate_tenant_overlap(false, &[&baseline], &[&expanded])
            .expect_err("new binding must not inherit VPC-wide grandfathering");

        // Restrictive policy rejects the new `ServiceVpcBinding`; permissive policy skips this check.
        assert!(matches!(error, crate::CarbideError::FailedPrecondition(_)));
        validate_tenant_overlap(true, &[&baseline], &[&expanded])
            .expect("explicit overlap setting permits the new binding");
    }

    /// Verifies one combined update cannot introduce a tenant interface and service endpoint in
    /// the same VPC because neither half exists in the grandfathering baseline.
    #[test]
    fn combined_new_host_and_service_bindings_are_rejected() {
        // Build the fully resolved result produced after explicit-prefix host allocation.
        let vpc_id = VpcId::new();
        let mut combined = InstanceNetworkConfig::for_segment_ids(
            &[NetworkSegmentId::new()],
            &[model::instance::config::network::DeviceLocator {
                device: "eth0".to_string(),
                device_instance: 0,
            }],
            &[vpc_id],
        );
        combined.service_interfaces.push(endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(0),
            vpc_id,
            0,
        ));
        let baseline = InstanceNetworkConfig::default();

        // Compare the combined result against the empty baseline network configuration.
        let error = validate_tenant_overlap(false, &[&baseline], &[&baseline, &combined])
            .expect_err("combined host and service additions must conflict");

        // The final combined-network check rejects what either isolated first-pass check could miss.
        assert!(matches!(error, crate::CarbideError::FailedPrecondition(_)));
    }

    /// Verifies an existing service binding blocks later PF and VF host bindings because overlap
    /// admission must work in both update directions and distinguish every host function.
    #[test]
    fn existing_service_binding_blocks_new_physical_and_virtual_host_bindings() {
        // Retain a service endpoint without host networking so each case adds only one host binding.
        let vpc_id = VpcId::new();
        let mut baseline = InstanceNetworkConfig::default();
        baseline.service_interfaces.push(endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(0),
            vpc_id,
            0,
        ));

        // A physical function is the ordinary reverse-order overlap that the service-first path must reject.
        let physical = InstanceNetworkConfig::for_segment_ids(
            &[NetworkSegmentId::new()],
            &[model::instance::config::network::DeviceLocator {
                device: "eth0".to_string(),
                device_instance: 0,
            }],
            &[vpc_id],
        );
        let physical_error = validate_tenant_overlap(false, &[&baseline], &[&baseline, &physical])
            .expect_err("a later physical host binding must conflict");
        assert!(matches!(
            physical_error,
            crate::CarbideError::FailedPrecondition(_)
        ));

        // A virtual function has a distinct `InterfaceFunctionId` but enforces the same VPC rule.
        let mut virtual_function = physical;
        virtual_function.interfaces[0].function_id = InterfaceFunctionId::Virtual { id: 0 };
        let virtual_error =
            validate_tenant_overlap(false, &[&baseline], &[&baseline, &virtual_function])
                .expect_err("a later virtual host binding must conflict");
        assert!(matches!(
            virtual_error,
            crate::CarbideError::FailedPrecondition(_)
        ));
    }

    /// Verifies pending and retained configurations both participate in overlap validation because
    /// either configuration can still describe live tenant connectivity during convergence.
    #[test]
    fn overlap_validation_includes_pending_and_retained_configurations() {
        // Keep the service interface in the persisted network config while a pending host update introduces its VPC.
        let vpc_id = VpcId::new();
        let mut retained_service = InstanceNetworkConfig::default();
        retained_service.service_interfaces.push(endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(0),
            vpc_id,
            0,
        ));
        let pending_host = InstanceNetworkConfig::for_segment_ids(
            &[NetworkSegmentId::new()],
            &[model::instance::config::network::DeviceLocator {
                device: "eth0".to_string(),
                device_instance: 0,
            }],
            &[vpc_id],
        );

        // Combining retained and pending views must reject the conflict before either becomes the sole view.
        let pending_error = validate_tenant_overlap(
            false,
            &[&retained_service],
            &[&retained_service, &pending_host],
        )
        .expect_err("pending host connectivity must conflict with a retained service endpoint");
        assert!(matches!(
            pending_error,
            crate::CarbideError::FailedPrecondition(_)
        ));

        // Keep the host interface in the persisted network config while a pending service interface introduces the inverse conflict.
        let mut pending_service = InstanceNetworkConfig::default();
        pending_service.service_interfaces.push(endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(0),
            vpc_id,
            0,
        ));
        let pending_service_error =
            validate_tenant_overlap(false, &[&pending_host], &[&pending_host, &pending_service])
                .expect_err(
                    "pending service connectivity must conflict with a retained host binding",
                );
        assert!(matches!(
            pending_service_error,
            crate::CarbideError::FailedPrecondition(_)
        ));
    }

    /// Verifies tightening overlap policy still permits removal and unrelated edits because only a
    /// newly introduced exact conflict should strand configurations accepted under the old policy.
    #[test]
    fn restrictive_overlap_policy_allows_removal_and_unrelated_edits() {
        // Build one grandfathered conflict that existed before the policy changed from true to false.
        let shared_vpc_id = VpcId::new();
        let mut baseline = InstanceNetworkConfig::for_segment_ids(
            &[NetworkSegmentId::new()],
            &[model::instance::config::network::DeviceLocator {
                device: "eth0".to_string(),
                device_instance: 0,
            }],
            &[shared_vpc_id],
        );
        baseline.service_interfaces.push(endpoint(
            model::test_support::machine_snapshot::dpu_machine_id(0),
            shared_vpc_id,
            0,
        ));

        // Removing the service half reduces the accepted conflict and therefore remains legal.
        let mut removed_service = baseline.clone();
        removed_service.service_interfaces.clear();
        validate_tenant_overlap(false, &[&baseline], &[&removed_service])
            .expect("removing a grandfathered overlap must remain possible");

        // Adding an unrelated host VPC preserves but does not expand the exact grandfathered conflict.
        let unrelated_vpc_id = VpcId::new();
        let unrelated_host = InstanceNetworkConfig::for_segment_ids(
            &[NetworkSegmentId::new()],
            &[model::instance::config::network::DeviceLocator {
                device: "eth1".to_string(),
                device_instance: 0,
            }],
            &[unrelated_vpc_id],
        );
        validate_tenant_overlap(false, &[&baseline], &[&baseline, &unrelated_host])
            .expect("an unrelated edit must not be blocked by grandfathered overlap");
    }

    /// Verifies removing a host binding in the requested result does not immediately free its VPC
    /// for a new service endpoint because the retained host path remains reachable until convergence.
    #[test]
    fn new_service_overlap_includes_retained_host_bindings() {
        // Retain one host binding while the requested host result removes every interface.
        let vpc_id = VpcId::new();
        let retained = InstanceNetworkConfig::for_segment_ids(
            &[NetworkSegmentId::new()],
            &[model::instance::config::network::DeviceLocator {
                device: "eth0".to_string(),
                device_instance: 0,
            }],
            &[vpc_id],
        );
        let requested = InstanceNetworkConfig::default();
        let intent = EndpointIntent {
            service_id: carbide_uuid::extension_service::ExtensionServiceId::new(),
            attachment_id: uuid::Uuid::new_v4(),
            interface_ordinal: 0,
            dpu_id: model::test_support::machine_snapshot::dpu_machine_id(0),
            vpc_id,
            family: ServiceVpcAddressFamily::Ipv4,
            mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
        };

        // Validate activation against both the requested and still-live host configurations.
        let error = validate_new_attachment_overlap(false, &requested, Some(&retained), &[intent])
            .expect_err("retained host connectivity must keep the overlap occupied");

        // Admission fails until host-network convergence removes the retained binding.
        assert!(matches!(error, crate::CarbideError::FailedPrecondition(_)));
    }
}
