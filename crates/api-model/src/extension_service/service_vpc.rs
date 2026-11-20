// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Durable endpoint identities and generation-correlated DPF evidence for service VPCs.

use std::collections::BTreeSet;

use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::machine::DpuMachineId;
use config_version::ConfigVersion;
use serde::{Deserialize, Serialize};

use super::ExtensionServiceChainSelector;
use crate::instance::config::extension_services::InstanceExtensionServiceConfig;
use crate::instance::config::network::InstanceServiceInterfaceConfig;
use crate::instance::snapshot::InstanceSnapshot;
use crate::instance::status::extension_service::ExtensionServiceDeploymentStatus;
use crate::machine::{DpuMachine, HostMachine, ManagedHostState, ReprovisionState};

/// Requires current applied network evidence so old Admin receipts cannot acknowledge secondary
/// service removal. Whole-instance retirement on primary Admin requires only the current host
/// receipt because that projection omits service endpoints; other paths require the current
/// Instance network generation and complete applied service UUID set.
pub fn network_acknowledged(
    host: &HostMachine,
    managed_state: &ManagedHostState,
    instance: &InstanceSnapshot,
    dpu: &DpuMachine,
    intent: ServiceVpcChainIntent,
) -> bool {
    let Some(network) = &dpu.network_status_observation else {
        return false;
    };
    if network.network_config_version != Some(host.network_config.version) {
        return false;
    }
    // Primary Admin projection always omits service endpoints, while secondary Admin
    // responses can retain them and therefore need the exact Instance removal receipt.
    if intent == ServiceVpcChainIntent::Retiring
        && (instance.deleted.is_some() || matches!(managed_state, ManagedHostState::Reset { .. }))
        && host.network_config.use_admin_network.unwrap_or(true)
        && host.primary_attached_dpu_machine_id() == Some(dpu.id)
    {
        return true;
    }
    let Some(service_network) = &network.service_vpc_network_observation else {
        return false;
    };
    if service_network.network_config_version != Some(instance.network_config_version) {
        return false;
    }

    // Compare the complete local UUID set because a generation alone cannot prove removal.
    let expected = instance
        .config
        .network
        .service_interfaces
        .iter()
        .filter(|endpoint| endpoint.dpu_id == dpu.id)
        .filter(|endpoint| {
            instance
                .config
                .extension_services
                .service_configs
                .iter()
                .any(|config| config.id == Some(endpoint.attachment_id) && config.removed.is_none())
        })
        .map(|endpoint| endpoint.internal_uuid)
        .collect::<BTreeSet<_>>();
    let reported = service_network
        .interfaces
        .iter()
        .map(|interface| interface.internal_uuid)
        .collect::<BTreeSet<_>>();
    expected == reported
}

/// Correlates remote observations with one management DPUCluster UID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DpuClusterIdentity {
    /// Management namespace containing the DPUCluster.
    pub namespace: String,
    /// Management DPUCluster name.
    pub name: String,
    /// UID of the DPUCluster whose remote resources may be observed.
    pub uid: String,
}

/// Records the remote Node incarnation that callers retain before enabling per-DPU ServiceChain creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceVpcNodeIdentity {
    /// Management DPUCluster incarnation serving this Node.
    pub cluster: DpuClusterIdentity,
    /// Remote Node name established from the management DPU mapping.
    pub name: String,
    /// Remote Node UID that subsequent convergence and withdrawal observations must match.
    pub uid: String,
}

/// Selects whether an observation checks current ServiceChain configuration or acknowledged withdrawal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceVpcChainIntent {
    /// Requires one current, reconciled ServiceChain on the retained Node.
    Active,
    /// Requires an acknowledged exclusion followed by authoritative ServiceChain absence.
    Retiring,
}

/// Correlates chain evidence with the caller's persisted attachment, endpoint and configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceVpcChainRequestContext {
    /// Server-owned attachment identity retained through termination.
    pub attachment_id: uuid::Uuid,
    /// InstanceServiceInterfaceConfig.internal_uuid for the endpoint being observed.
    pub internal_uuid: uuid::Uuid,
    /// Registered extension service that owns the chain.
    pub service_id: ExtensionServiceId,
    /// Zero-based registered service-interface ordinal.
    pub interface_ordinal: u32,
    /// Fixed local service-VPC slot selected for this endpoint.
    pub slot_index: u32,
    /// Persisted DPU identity for this endpoint.
    pub dpu_id: DpuMachineId,
    /// Version of the instance's complete extension-services configuration.
    pub extension_services_config_version: ConfigVersion,
    /// Version of the instance network configuration containing this endpoint.
    pub network_config_version: ConfigVersion,
    /// Committed extension_service_chain_selectors.version used by the requested definition.
    pub selector_version: ConfigVersion,
    /// Persisted activation or retirement desire for this observation.
    pub intent: ServiceVpcChainIntent,
}

/// Records the identity and generation used by a chain observation without retaining a raw spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceVpcResourceObservation {
    /// Namespace containing the observed resource.
    pub namespace: String,
    /// Observed Kubernetes name.
    pub name: String,
    /// Observed Kubernetes UID.
    pub uid: String,
    /// Current metadata.generation used when checking condition acknowledgements.
    pub generation: i64,
    /// Resource version of this exact API observation.
    pub resource_version: String,
    /// Whether Kubernetes has accepted deletion of this resource.
    pub is_deleting: bool,
}

/// Retains an endpoint's Node identity and current placement, network, and chain evidence so
/// retries and process restarts cannot reclaim a different attachment or Node incarnation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceVpcEndpointStatusObservation {
    /// Complete persisted endpoint whose allocation this observation can describe.
    pub endpoint: InstanceServiceInterfaceConfig,
    /// Attachment, endpoint, selector, and configuration versions observed together.
    pub context: ServiceVpcChainRequestContext,
    /// Managed-host network generation used when checking the DPU's acknowledgement.
    pub managed_host_config_version: ConfigVersion,
    /// Node incarnation retained before authorizing placement and preserved through retirement.
    pub node: Option<ServiceVpcNodeIdentity>,
    /// Sticky evidence of unsupported finalizer bypass; only administrative recovery may discard it.
    pub cleanup_bypass_observed: bool,
    /// DPUDevice read back after this generation's successful placement patch.
    pub placement: Option<ServiceVpcResourceObservation>,
    /// Both required labels match this endpoint's activation or withdrawal desire.
    pub placement_observed: bool,
    /// HBN application and DHCP submission acknowledge the required network generation.
    pub network_acknowledged: bool,
    /// The typed DPF observer established current positive ServiceChain conditions.
    pub chain_converged: bool,
    /// The typed DPF observer established acknowledged exclusion followed by fresh chain absence.
    pub chain_isolated: bool,
    /// Management registration incarnation validated by the typed DPF observer.
    pub registration: Option<ServiceVpcResourceObservation>,
    /// Remote set incarnation and generation validated before the chain list.
    pub set: Option<ServiceVpcResourceObservation>,
    /// Relevant chain incarnations returned by the typed DPF observer.
    pub chains: Vec<ServiceVpcResourceObservation>,
    /// Complete chain-list resource version read after the set acknowledgement.
    pub chain_list_resource_version: Option<String>,
    /// Pending or error diagnostic for this endpoint, without claiming workload health.
    pub message: String,
}

impl ServiceVpcEndpointStatusObservation {
    /// Matches the complete allocation and both configuration versions so stale endpoint evidence
    /// cannot satisfy another attachment, activation, or removal.
    pub fn matches_endpoint(
        &self,
        endpoint: &InstanceServiceInterfaceConfig,
        attachment: &InstanceExtensionServiceConfig,
        extension_version: ConfigVersion,
        network_version: ConfigVersion,
    ) -> bool {
        self.endpoint == *endpoint
            && attachment.id == Some(self.context.attachment_id)
            && self.context.attachment_id == endpoint.attachment_id
            && self.context.internal_uuid == endpoint.internal_uuid
            && self.context.service_id == attachment.service_id
            && self.context.interface_ordinal == endpoint.interface_ordinal
            && self.context.slot_index == endpoint.slot_index
            && self.context.dpu_id == endpoint.dpu_id
            && self.context.extension_services_config_version == extension_version
            && self.context.network_config_version == network_version
            && self.context.intent
                == if attachment.removed.is_some() {
                    ServiceVpcChainIntent::Retiring
                } else {
                    ServiceVpcChainIntent::Active
                }
    }

    /// Checks current desired generations, the latest DPU receipt and selector membership so
    /// retained chain evidence cannot publish completion after a host or shared selector changes,
    /// or activation while this DPU is reprovisioning.
    /// This checks freshness independently of the observation's conclusive flags.
    pub fn matches_current_context(
        &self,
        host: &HostMachine,
        managed_state: &ManagedHostState,
        instance: &InstanceSnapshot,
        dpu: &DpuMachine,
        selector: &ExtensionServiceChainSelector,
    ) -> bool {
        self.matches_desired_context(host, instance, dpu, selector)
            && network_acknowledged(host, managed_state, instance, dpu, self.context.intent)
    }

    /// Matches the persisted endpoint, host network generation and selector desire so current
    /// placement and chain evidence remain observable while network acknowledgement is pending.
    /// Active evidence is rejected while this DPU has a started reprovision request or an
    /// in-progress persisted reprovision state.
    pub fn matches_desired_context(
        &self,
        host: &HostMachine,
        instance: &InstanceSnapshot,
        dpu: &DpuMachine,
        selector: &ExtensionServiceChainSelector,
    ) -> bool {
        // Block activation during persisted DPU reprovisioning, including DPF deployment migration's
        // NotUnderReprovision checkpoint with a started request; retirement remains eligible.
        if self.context.intent == ServiceVpcChainIntent::Active
            && (dpu
                .reprovision_requested
                .as_ref()
                .is_some_and(|request| request.started_at.is_some())
                || !matches!(
                    host.state.value.as_reprovision_state(&dpu.id),
                    None | Some(ReprovisionState::NotUnderReprovision)
                ))
        {
            return false;
        }

        // Resolve the retained attachment because its removal marker determines selector membership.
        let Some(attachment) = instance
            .config
            .extension_services
            .service_configs
            .iter()
            .find(|attachment| attachment.id == Some(self.endpoint.attachment_id))
        else {
            return false;
        };
        instance
            .config
            .network
            .service_interfaces
            .contains(&self.endpoint)
            && self.matches_endpoint(
                &self.endpoint,
                attachment,
                instance.extension_services_config_version,
                instance.network_config_version,
            )
            && dpu.id == self.endpoint.dpu_id
            && self.managed_host_config_version == host.network_config.version
            && selector.service_id == self.context.service_id
            && selector.interface_ordinal == self.context.interface_ordinal
            && selector.slot == self.context.slot_index
            && selector.version == self.context.selector_version
            && selector.excluded_dpu_ids.contains(&dpu.id)
                == (self.context.intent == ServiceVpcChainIntent::Retiring)
    }

    /// Requires placement, network acknowledgement, and typed chain evidence together so neither
    /// labels nor an old empty chain list can independently establish convergence or reclamation.
    pub fn deployment_status(&self) -> ExtensionServiceDeploymentStatus {
        let complete = self.node.is_some()
            && !self.cleanup_bypass_observed
            && self.placement_observed
            && self
                .placement
                .as_ref()
                .is_some_and(|resource| !resource.is_deleting)
            && self.network_acknowledged
            && self
                .registration
                .as_ref()
                .is_some_and(|resource| !resource.is_deleting)
            && self
                .set
                .as_ref()
                .is_some_and(|resource| !resource.is_deleting)
            && self.chain_list_resource_version.is_some();
        match self.context.intent {
            ServiceVpcChainIntent::Active if complete && self.chain_converged => {
                ExtensionServiceDeploymentStatus::Running
            }
            ServiceVpcChainIntent::Retiring if complete && self.chain_isolated => {
                ExtensionServiceDeploymentStatus::Terminated
            }
            ServiceVpcChainIntent::Active => ExtensionServiceDeploymentStatus::Pending,
            ServiceVpcChainIntent::Retiring => ExtensionServiceDeploymentStatus::Terminating,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbide_test_support::value_scenarios;
    use config_version::Versioned;

    use super::*;
    use crate::instance::config::extension_services::InstanceExtensionServicesConfig;
    use crate::instance::config::network::InstanceNetworkConfig;
    use crate::instance::status::extension_service::{
        InstanceExtensionServiceStatusObservation, InstanceExtensionServiceStatusObservationByType,
        InstanceExtensionServicesStatus,
    };

    /// Builds a complete synthetic observation so tests can remove one required proof while
    /// retaining valid endpoint identities and independent extension/network configuration versions.
    fn complete_observation() -> ServiceVpcEndpointStatusObservation {
        let attachment_id = uuid::uuid!("10000000-0000-0000-0000-000000000018");
        let internal_uuid = uuid::uuid!("20000000-0000-0000-0000-000000000018");
        let dpu_id = "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg"
            .parse()
            .expect("DPU ID");
        let resource = |name: &str| ServiceVpcResourceObservation {
            namespace: "service-vpc".to_owned(),
            name: name.to_owned(),
            uid: format!("{name}-uid"),
            generation: 7,
            resource_version: "19".to_owned(),
            is_deleting: false,
        };
        // These fields supply one complete allocation; the tests vary evidence rather than IPAM.
        let endpoint = InstanceServiceInterfaceConfig {
            attachment_id,
            interface_ordinal: 0,
            dpu_id,
            slot_index: 0,
            vpc_id: "30000000-0000-0000-0000-000000000018"
                .parse()
                .expect("VPC ID"),
            vpc_prefix_id: "40000000-0000-0000-0000-000000000018"
                .parse()
                .expect("VPC prefix ID"),
            network_segment_id: "50000000-0000-0000-0000-000000000018"
                .parse()
                .expect("segment ID"),
            network_prefix_id: "60000000-0000-0000-0000-000000000018"
                .parse()
                .expect("network prefix ID"),
            link_prefix: "192.0.2.0/31".parse().expect("canonical link"),
            mac_address: "02:00:00:00:00:18".parse().expect("service MAC"),
            internal_uuid,
        };
        ServiceVpcEndpointStatusObservation {
            endpoint,
            context: ServiceVpcChainRequestContext {
                attachment_id,
                internal_uuid,
                service_id: "70000000-0000-0000-0000-000000000018"
                    .parse()
                    .expect("service ID"),
                interface_ordinal: 0,
                slot_index: 0,
                dpu_id,
                extension_services_config_version: "V3-T3".parse().expect("extension version"), // Distinguish the two required generations.
                network_config_version: "V4-T4".parse().expect("network version"),
                selector_version: "V5-T5".parse().expect("selector version"),
                intent: ServiceVpcChainIntent::Active,
            },
            managed_host_config_version: "V6-T6".parse().expect("host version"),
            node: Some(ServiceVpcNodeIdentity {
                cluster: DpuClusterIdentity {
                    namespace: "service-vpc".to_owned(),
                    name: "cluster".to_owned(),
                    uid: "cluster-uid".to_owned(),
                },
                name: "node".to_owned(),
                uid: "node-uid".to_owned(),
            }),
            cleanup_bypass_observed: false,
            placement: Some(resource("device")),
            placement_observed: true, // Supply both required placement labels.
            network_acknowledged: true, // Supply HBN/DHCP acknowledgement independently of chain state.
            chain_converged: true,      // Supply the current typed chain result.
            chain_isolated: false,
            registration: Some(resource("registration")),
            set: Some(resource("set")),
            chains: vec![resource("chain")],
            chain_list_resource_version: Some("after-set-ack".to_owned()),
            message: String::new(),
        }
    }

    /// Withholds Running and Terminated when one required proof is incomplete, preventing labels,
    /// incomplete chain observations, or an old network report from independently releasing capacity.
    #[test]
    fn endpoint_convergence_requires_complete_current_evidence() {
        #[derive(Clone, Copy)]
        enum Change {
            Complete,
            MissingPlacement,
            StaleChain,
            UnacknowledgedNetwork,
            IncompleteIsolation,
            MissingChainList,
            FinalizerBypass,
        }
        use Change::*;
        use ExtensionServiceDeploymentStatus::*;
        use ServiceVpcChainIntent::*;

        value_scenarios!(run = |(intent, change)| {
            // Start from complete evidence and select activation or withdrawal before varying one proof.
            let mut observed = complete_observation();
            observed.context.intent = intent;
            if intent == Retiring {
                observed.chain_converged = false;
                observed.chain_isolated = true;
                observed.chains.clear();
            }
            // Remove only the requirement named by this case to isolate its lifecycle consequence.
            match change {
                Complete => {}
                MissingPlacement => observed.placement_observed = false,
                StaleChain => observed.chain_converged = false,
                UnacknowledgedNetwork => observed.network_acknowledged = false,
                IncompleteIsolation => observed.chain_isolated = false,
                MissingChainList => observed.chain_list_resource_version = None,
                FinalizerBypass => observed.cleanup_bypass_observed = true,
            }
            observed.deployment_status()
        };
            // A missing placement label prevents positive chain evidence from authorizing Running.
            "missing label" { (Active, MissingPlacement) => Pending, }
            // Old chain conditions cannot borrow a current network acknowledgement.
            "stale chain" { (Active, StaleChain) => Pending, }
            // Chain convergence alone cannot prove HBN application and DHCP submission.
            "unacknowledged network" { (Active, UnacknowledgedNetwork) => Pending, }
            // Every independent active requirement together establishes current convergence.
            "complete activation" { (Active, Complete) => Running, }
            // A current list without typed isolation cannot authorize reclamation.
            "incomplete chain isolation" { (Retiring, IncompleteIsolation) => Terminating, }
            // Typed isolation without the required chain-list observation remains incomplete.
            "missing chain-list observation" { (Retiring, MissingChainList) => Terminating, }
            // Complete chain isolation still requires acknowledgement of HBN/DHCP removal.
            "unacknowledged removal" { (Retiring, UnacknowledgedNetwork) => Terminating, }
            // Finalizer bypass stays incomplete even after a later empty list.
            "cleanup bypass" { (Retiring, FinalizerBypass) => Terminating, }
            // Current withdrawal, isolation, and network acknowledgement together permit final cleanup.
            "complete retirement" { (Retiring, Complete) => Terminated, }
        );
    }

    /// Keeps both persisted endpoint DPUs required after discovery loss and rejects stale network
    /// or extension generations and mismatched attachment or endpoint identities, preventing host-network
    /// target changes or another allocation's evidence from hiding incomplete DPUs.
    #[test]
    fn aggregate_status_uses_fixed_endpoint_dpus_and_both_versions() {
        #[derive(Clone, Copy)]
        enum Change {
            Complete,
            MissingDpu,
            OldNetwork,
            OldExtensions,
            WrongAttachment,
            WrongEndpoint,
        }
        use Change::*;
        use ExtensionServiceDeploymentStatus::*;

        value_scenarios!(run = |change| {
            // Build two independently identified endpoint proofs for one accepted attachment.
            let first = complete_observation();
            let mut second = first.clone();
            second.endpoint.dpu_id = "fm100ds7blqjsadm2uuh3qqbf1h7k8pmf47um6v9uckrg7l03po8mhqgvng".parse().expect("second DPU");
            second.context.dpu_id = second.endpoint.dpu_id;
            second.endpoint.internal_uuid = uuid::uuid!("20000000-0000-0000-0000-000000000019");
            second.context.internal_uuid = second.endpoint.internal_uuid;
            let mut attached = vec![first.endpoint.dpu_id, second.endpoint.dpu_id];
            let config = InstanceExtensionServicesConfig { service_configs: vec![
                InstanceExtensionServiceConfig {
                    id: Some(first.context.attachment_id),
                    service_id: first.context.service_id,
                    version: "V1-T1".parse().expect("registered service version"),
                    // Keep AllActive independent of the empty host-network target set below.
                    dpu_target: Some(crate::extension_service::DpuTarget::AllActive),
                    removed: None,
                },
            ] };
            let network = InstanceNetworkConfig {
                service_interfaces: vec![first.endpoint.clone(), second.endpoint.clone()],
                ..Default::default()
            };

            // Leave the complete endpoint set stored while changing visibility, one token, or one observation identity.
            match change {
                Complete => {}
                MissingDpu => { attached.pop(); }
                OldNetwork => second.context.network_config_version = "V2-T2".parse().expect("old network"),
                OldExtensions => second.context.extension_services_config_version = "V2-T2".parse().expect("old extensions"),
                WrongAttachment => second.context.attachment_id = uuid::uuid!("10000000-0000-0000-0000-000000000019"),
                WrongEndpoint => second.context.internal_uuid = first.context.internal_uuid,
            }
            let mut observations = HashMap::new();
            for evidence in [&first, &second] {
                let mut typed = InstanceExtensionServiceStatusObservationByType::default();
                typed.set_for_service_type(crate::extension_service::ExtensionServiceType::DpfHelmChart,
                    InstanceExtensionServiceStatusObservation {
                        config_version: first.context.extension_services_config_version,
                        instance_config_version: None,
                        extension_service_statuses: Vec::new(),
                        observed_at: chrono::Utc::now(),
                        service_vpc_endpoints: vec![evidence.clone()],
                    });
                observations.insert(evidence.endpoint.dpu_id, typed);
            }
            // An empty host-network target set must not narrow the service's persisted DPU set.
            let status = InstanceExtensionServicesStatus::from_config_and_type_observations(
                &[], &attached, Some(first.endpoint.dpu_id),
                Versioned::new(&config, first.context.extension_services_config_version),
                &observations, false,
                Some(Versioned::new(&network, first.context.network_config_version)),
            );
            assert_eq!(status.extension_services[0].dpu_statuses.len(), 2);
            status.extension_services[0].overall_status.clone()
        };
            // Both persisted DPUs can converge independently of current host-network placement.
            "complete fixed endpoints" { Complete => Running, }
            // Even retained positive observations cannot conceal a missing discovery record.
            "missing endpoint DPU" { MissingDpu => Pending, }
            // Current placement cannot reuse a prior network generation.
            "old network generation" { OldNetwork => Pending, }
            // A matching network version cannot reuse a prior attachment configuration.
            "old extension generation" { OldExtensions => Pending, }
            // Matching generations cannot make another attachment's context describe this allocation.
            "wrong attachment at current generations" { WrongAttachment => Pending, }
            // Same-attachment endpoints on different DPUs still require their own internal UUIDs.
            "wrong endpoint at current generations" { WrongEndpoint => Pending, }
        );
    }
}
