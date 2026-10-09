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

//! NICo-managed service-VPC slot topology.
//!
//! Each configured slot `N` reserves the isolated OVS bridge `br-svc-N`
//! as a stable connection point between HBN and a DPU service dynamically
//! enabled after DPU provisioning.
//!
//! NICo creates these resources for slot `N`:
//!
//! ```text
//! DPUFlavor
//!   OVS initialization creates br-svc-N
//!
//! DPUServiceConfiguration/doca-hbn
//!   interfaces:
//!     - name: iface_svc_N
//!
//! DPUServiceConfiguration/carbide-dhcp-server
//!   interfaces:
//!     - name: d_iface_svc_N
//!       network: service-vpc-dhcp-slotN
//!
//! DPUServiceNAD/service-vpc-dhcp-slotN
//!   bridge: br-svc-N
//!   resourceType: sf
//!   ipam: false
//!   serviceMTU: 1500
//!
//! DPUServiceInterface/service-vpc-slot-N
//!   label: interface=service-vpc-slot-N
//!   type: Patch
//!   peerBridge: br-svc-N
//!   peerPatchName: svc-slot-N
//!
//! DPUDeployment/<provisioning deployment>
//!   serviceChains.switches[N].ports:
//!     - service: doca-hbn/iface_svc_N
//!     - serviceInterface: interface=service-vpc-slot-N
//! ```
//!
//! DPF materializes that desired state on each selected DPU. `br-sfc` is DPF's
//! shared OVS service-function-chaining bridge. The Patch interface causes DPF's
//! SFC controller to create both OVS patch ports; `p_brsfc_to_svc-slot-N` is
//! derived from the explicitly configured peer name `svc-slot-N`.
//!
//! ```text
//! doca-hbn/iface_svc_N
//!          |
//! DPUServiceChain switch on br-sfc
//!          |
//! p_brsfc_to_svc-slot-N <========> svc-slot-N
//!                                      |
//!                                  br-svc-N
//!                                      |
//!            post-provisioning dynamically enabled DPU service
//! ```
//!
//! The dynamically enabled service creates its own Patch `DPUServiceInterface`
//! terminating on `br-svc-N` and its own `DPUServiceChain` connecting that patch
//! to the service interface. The fixed DHCP listener attaches directly to the slot
//! bridge through its NAD and does not add a service-chain endpoint.

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::error::DpfError;
use crate::types::{
    DHCP_SERVER_SERVICE_NAME, DOCA_HBN_SERVICE_NAME, DOCA_HBN_SERVICE_NETWORK,
    DpuServiceInterfacePatch, DpuServiceInterfaceTemplateDefinition,
    DpuServiceInterfaceTemplateType, ServiceDefinition, ServiceInterface, ServiceNAD,
    ServiceNADResourceType,
};

pub(crate) const MAX_HBN_SERVICE_INTERFACES: usize = 32;

/// MTU shared by fixed service-VPC listeners and their service-pod attachment networks.
pub const SERVICE_VPC_MTU: i64 = 1500;

/// A validated collection of NICo-managed service-VPC slots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServiceVpcSlots {
    count: u32,
}

impl ServiceVpcSlots {
    /// Validates and constructs a service-VPC slot collection.
    pub fn new(count: u32) -> Result<Self, DpfError> {
        if count > MAX_HBN_SERVICE_INTERFACES as u32 {
            return Err(DpfError::ConfigError(format!(
                "service-VPC slot count {count} exceeds the supported HBN interface maximum of {MAX_HBN_SERVICE_INTERFACES}"
            )));
        }
        Ok(Self { count })
    }

    /// Returns whether no service-VPC slots are configured.
    pub(crate) const fn is_empty(self) -> bool {
        self.count == 0
    }

    /// Appends this feature's HBN interfaces to the caller's base HBN inventory.
    pub fn append_hbn_interfaces(self, interfaces: &mut Vec<ServiceInterface>) {
        interfaces.extend(self.slots().map(|slot| ServiceInterface {
            name: slot.hbn_interface_name(),
            network: DOCA_HBN_SERVICE_NETWORK.to_string(),
        }));
    }

    /// Returns a configured slot's local bridge for registration topology, or None out of range.
    pub fn bridge_name(self, index: u32) -> Option<String> {
        (index < self.count).then(|| ServiceVpcSlot(index).bridge_name())
    }

    /// Appends one IPAM-disabled SF listener and its bridge-specific NAD per slot.
    /// These server interfaces receive exact reservations from NICo rather than requesting CNI leases.
    pub fn append_dhcp_interfaces(self, service: &mut ServiceDefinition) {
        // Keep the netdev and NAD identities together so attachment references cannot drift.
        for slot in self.slots() {
            service.interfaces.push(ServiceInterface {
                name: slot.dhcp_interface_name(),
                network: slot.dhcp_nad_name(),
            });
            service.service_nads.push(ServiceNAD {
                name: slot.dhcp_nad_name(),
                bridge: Some(slot.bridge_name()),
                resource_type: ServiceNADResourceType::Sf,
                ipam: Some(false),
                mtu: Some(SERVICE_VPC_MTU),
            });
        }
    }

    /// Appends the dedicated OVS bridge for every configured slot.
    pub(crate) fn append_ovs_bridges(self, script: &mut String) {
        // Reprovision performs a full OS installation, so OVSDB is reset before this script runs;
        // bridges from a previous, larger slot count do not need explicit deletion here.
        for slot in self.slots() {
            // Each slot bridge is dedicated to one HBN patch, its DHCP SF, and consumer service patches.
            let bridge = slot.bridge_name();
            let _ = writeln!(
                script,
                "_ovs-vsctl --may-exist add-br {bridge}\n_ovs-vsctl set bridge {bridge} datapath_type=netdev\n_ovs-vsctl set bridge {bridge} fail_mode=standalone"
            );
        }
    }

    /// Validates and adds the complete slot topology to initialization state.
    pub(crate) fn apply(
        self,
        services: &[ServiceDefinition],
        interfaces: &mut Vec<DpuServiceInterfaceTemplateDefinition>,
    ) -> Result<(), DpfError> {
        if self.is_empty() {
            return Ok(());
        }

        let hbn_interface_names = interfaces
            .iter()
            .flat_map(|interface| interface.chained_svc_if.iter().flatten())
            .filter(|(service, _)| service == DOCA_HBN_SERVICE_NAME)
            .map(|(_, name)| name.as_str())
            .collect::<BTreeSet<_>>();
        let remaining_hbn_interfaces =
            MAX_HBN_SERVICE_INTERFACES.saturating_sub(hbn_interface_names.len());
        if self.count > remaining_hbn_interfaces as u32 {
            return Err(DpfError::ConfigError(format!(
                "service-VPC slot count {} exceeds the remaining HBN interface capacity of {remaining_hbn_interfaces}",
                self.count,
            )));
        }

        let interface_names = interfaces
            .iter()
            .map(|interface| interface.name.as_str())
            .collect::<BTreeSet<_>>();
        let mut ovs_names = BTreeSet::new();
        for interface in interfaces.iter() {
            match &interface.iface_type {
                DpuServiceInterfaceTemplateType::Physical => {
                    ovs_names.insert(interface.name.clone());
                }
                DpuServiceInterfaceTemplateType::Patch(patch) => {
                    ovs_names.insert(patch.peer_bridge.clone());
                    ovs_names.insert(patch.peer_patch_name.clone());
                    ovs_names.insert(format!("p_brsfc_to_{}", patch.peer_patch_name));
                }
                _ => {}
            }
        }

        for slot in self.slots() {
            let peer_patch_name = slot.peer_patch_name();
            let slot_ovs_names = [
                slot.bridge_name(),
                peer_patch_name.clone(),
                format!("p_brsfc_to_{peer_patch_name}"),
            ];
            if interface_names.contains(slot.interface_name().as_str())
                || hbn_interface_names.contains(slot.hbn_interface_name().as_str())
                || interfaces
                    .iter()
                    .flat_map(|interface| interface.chained_svc_if.iter().flatten())
                    .any(|(service, name)| {
                        service == DHCP_SERVER_SERVICE_NAME && name == &slot.dhcp_interface_name()
                    })
                || slot_ovs_names.iter().any(|name| ovs_names.contains(name))
            {
                return Err(DpfError::ConfigError(format!(
                    "DPF interface or OVS name conflicts with reserved service-VPC slot {}",
                    slot.interface_name(),
                )));
            }
        }
        let slot_interfaces = self
            .slots()
            .map(ServiceVpcSlot::dpu_interface)
            .collect::<Vec<_>>();

        let mut hbn_services = services
            .iter()
            .filter(|service| service.name == DOCA_HBN_SERVICE_NAME);
        let hbn = hbn_services.next().ok_or_else(|| {
            DpfError::ConfigError(
                "service-VPC slots require a doca-hbn service definition".to_string(),
            )
        })?;
        if hbn_services.next().is_some() {
            return Err(DpfError::ConfigError(
                "service-VPC slots require exactly one doca-hbn service definition".to_string(),
            ));
        }

        let mut expected = interfaces
            .iter()
            .chain(&slot_interfaces)
            .flat_map(|interface| interface.chained_svc_if.iter().flatten())
            .filter(|(service, _)| service == DOCA_HBN_SERVICE_NAME)
            .map(|(_, name)| (name.clone(), DOCA_HBN_SERVICE_NETWORK.to_string()))
            .collect::<Vec<_>>();
        expected.sort_unstable();
        let mut actual = hbn
            .interfaces
            .iter()
            .map(|interface| (interface.name.clone(), interface.network.clone()))
            .collect::<Vec<_>>();
        actual.sort_unstable();
        if actual != expected {
            return Err(DpfError::ConfigError(
                "doca-hbn interface inventory must exactly match the resolved DPF HBN chains"
                    .to_string(),
            ));
        }
        // A slot is incomplete without its direct DHCP SF, even though that endpoint has no chain.
        let mut dhcp_services = services
            .iter()
            .filter(|service| service.name == DHCP_SERVER_SERVICE_NAME);
        let dhcp = dhcp_services.next().ok_or_else(|| {
            DpfError::ConfigError("service-VPC slots require a DHCP service definition".to_string())
        })?;
        if dhcp_services.next().is_some() {
            return Err(DpfError::ConfigError(
                "service-VPC slots require exactly one DHCP service definition".to_string(),
            ));
        }
        for slot in self.slots() {
            let name = slot.dhcp_interface_name();
            let network = slot.dhcp_nad_name();
            let mut listeners = dhcp
                .interfaces
                .iter()
                .filter(|interface| interface.name == name);
            let listener_matches = listeners
                .next()
                .is_some_and(|interface| interface.network == network)
                && listeners.next().is_none();
            // NAD names are namespace-wide, so another service cannot reuse the slot's NAD name.
            let mut nads = services
                .iter()
                .flat_map(|service| &service.service_nads)
                .filter(|nad| nad.name == network);
            let nad_matches = nads.next().is_some_and(|nad| {
                nad.bridge.as_deref() == Some(slot.bridge_name().as_str())
                    && matches!(nad.resource_type, ServiceNADResourceType::Sf)
                    && nad.ipam == Some(false)
                    && nad.mtu == Some(SERVICE_VPC_MTU)
            }) && nads.next().is_none();
            if !listener_matches || !nad_matches {
                return Err(DpfError::ConfigError(format!(
                    "service-VPC slot {name} requires exactly one DHCP interface and an IPAM-disabled SF NAD on its bridge with MTU {SERVICE_VPC_MTU}"
                )));
            }
        }

        interfaces.extend(slot_interfaces);
        Ok(())
    }

    fn slots(self) -> impl Iterator<Item = ServiceVpcSlot> {
        (0..self.count).map(ServiceVpcSlot)
    }
}

#[derive(Clone, Copy)]
struct ServiceVpcSlot(u32);

impl ServiceVpcSlot {
    fn interface_name(self) -> String {
        format!("service-vpc-slot-{}", self.0)
    }

    fn bridge_name(self) -> String {
        format!("br-svc-{}", self.0)
    }

    fn peer_patch_name(self) -> String {
        format!("svc-slot-{}", self.0)
    }

    fn hbn_interface_name(self) -> String {
        format!("iface_svc_{}", self.0)
    }

    /// Matches the agent's existing HBN-to-DHCP netdev translation.
    fn dhcp_interface_name(self) -> String {
        format!("d_{}", self.hbn_interface_name())
    }

    /// Keeps each server listener attached to its own slot bridge.
    fn dhcp_nad_name(self) -> String {
        format!("service-vpc-dhcp-slot{}", self.0)
    }

    fn dpu_interface(self) -> DpuServiceInterfaceTemplateDefinition {
        DpuServiceInterfaceTemplateDefinition {
            name: self.interface_name(),
            iface_type: DpuServiceInterfaceTemplateType::Patch(DpuServiceInterfacePatch {
                peer_bridge: self.bridge_name(),
                peer_patch_name: self.peer_patch_name(),
                peer_external_ids: None,
            }),
            pf_id: 0,
            vf_id: 0,
            chained_svc_if: Some(vec![(
                DOCA_HBN_SERVICE_NAME.to_string(),
                self.hbn_interface_name(),
            )]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physical_interface(
        name: &str,
        hbn_interface: Option<&str>,
    ) -> DpuServiceInterfaceTemplateDefinition {
        DpuServiceInterfaceTemplateDefinition {
            name: name.to_string(),
            iface_type: DpuServiceInterfaceTemplateType::Physical,
            pf_id: 0,
            vf_id: 0,
            chained_svc_if: hbn_interface
                .map(|name| vec![(DOCA_HBN_SERVICE_NAME.to_string(), name.to_string())]),
        }
    }

    fn hbn_service(interfaces: Vec<ServiceInterface>) -> ServiceDefinition {
        ServiceDefinition {
            interfaces,
            ..ServiceDefinition::new(DOCA_HBN_SERVICE_NAME, "repo", "chart", "1")
        }
    }

    /// Supplies the fixed DHCP attachment so topology tests cannot accept HBN-only slots.
    fn dhcp_service(slots: ServiceVpcSlots) -> ServiceDefinition {
        let mut service = ServiceDefinition::new(DHCP_SERVER_SERVICE_NAME, "repo", "chart", "1");
        slots.append_dhcp_interfaces(&mut service);
        service
    }

    /// Verifies slot names, bridges and both service attachments agree so empty slots are usable.
    #[test]
    fn generates_complete_slot_topology() {
        // Two slots expose ordering and prevent accidental bridge sharing.
        let slots = ServiceVpcSlots::new(2).expect("valid slot count");
        let mut interfaces = Vec::new();
        let mut hbn_interfaces = Vec::new();
        slots.append_hbn_interfaces(&mut hbn_interfaces);
        slots
            .apply(
                &[hbn_service(hbn_interfaces.clone()), dhcp_service(slots)],
                &mut interfaces,
            )
            .unwrap();
        let mut ovs = String::new();
        slots.append_ovs_bridges(&mut ovs);

        // The bounded accessor shares bridge identities with dynamic registration.
        assert_eq!(slots.bridge_name(1).as_deref(), Some("br-svc-1"));
        assert_eq!(slots.bridge_name(2), None);

        // Existing HBN names and bridge bootstrap remain unchanged.
        assert_eq!(
            hbn_interfaces
                .into_iter()
                .map(|interface| interface.name)
                .collect::<Vec<_>>(),
            ["iface_svc_0", "iface_svc_1"]
        );
        assert_eq!(
            interfaces
                .iter()
                .map(|interface| interface.name.as_str())
                .collect::<Vec<_>>(),
            ["service-vpc-slot-0", "service-vpc-slot-1"]
        );
        assert!(matches!(
            &interfaces[0].iface_type,
            DpuServiceInterfaceTemplateType::Patch(patch)
                if patch.peer_bridge == "br-svc-0" && patch.peer_patch_name == "svc-slot-0"
        ));
        assert_eq!(
            ovs,
            concat!(
                "_ovs-vsctl --may-exist add-br br-svc-0\n",
                "_ovs-vsctl set bridge br-svc-0 datapath_type=netdev\n",
                "_ovs-vsctl set bridge br-svc-0 fail_mode=standalone\n",
                "_ovs-vsctl --may-exist add-br br-svc-1\n",
                "_ovs-vsctl set bridge br-svc-1 datapath_type=netdev\n",
                "_ovs-vsctl set bridge br-svc-1 fail_mode=standalone\n",
            )
        );
    }

    /// Verifies the HBN side remains in chained SF accounting; DHCP is counted from NAD inventory.
    #[test]
    fn generated_slot_participates_in_sf_capacity() {
        // Generate a complete slot so its HBN endpoint enters the chained SF inventory.
        let slots = ServiceVpcSlots::new(1).unwrap();
        let mut hbn_interfaces = Vec::new();
        slots.append_hbn_interfaces(&mut hbn_interfaces);
        let mut interfaces = Vec::new();
        slots
            .apply(
                &[hbn_service(hbn_interfaces), dhcp_service(slots)],
                &mut interfaces,
            )
            .unwrap();

        // The HBN chain consumes one SF; the SDK counts its separate DHCP attachment from NAD inventory.
        assert!(crate::calculate_pf_total_sf(&interfaces, None, 0, 0).is_err());
        assert_eq!(
            crate::calculate_pf_total_sf(&interfaces, None, 1, 0).unwrap(),
            1
        );
    }

    /// Verifies invalid DHCP attachments fail before slot topology is added, protecting isolation and leases.
    #[test]
    fn rejects_inconsistent_dhcp_slot_inventory() {
        type DhcpMutation = fn(&mut ServiceDefinition);

        // All fields except each case's mutation provide a valid one-slot service inventory.
        let slots = ServiceVpcSlots::new(1).expect("valid slot inventory");
        let mut hbn_interfaces = Vec::new();
        slots.append_hbn_interfaces(&mut hbn_interfaces);
        let cases: &[(&str, DhcpMutation)] = &[
            // A missing listener leaves an otherwise provisioned slot unable to serve DHCP.
            ("missing listener", |service| service.interfaces.clear()),
            // Duplicate listener names must not conceal a second attachment to the slot.
            ("duplicate listener", |service| {
                service.interfaces.push(service.interfaces[0].clone())
            }),
            // A listener without its NAD has no defined SF attachment to the slot bridge.
            ("missing NAD", |service| service.service_nads.clear()),
            // A name collision must not silently overwrite the slot's namespace-wide NAD.
            ("duplicate NAD", |service| {
                service.service_nads.push(service.service_nads[0].clone())
            }),
            // A wrong network detaches the listener from its designated slot bridge.
            ("wrong network", |service| {
                service.interfaces[0].network = "wrong".to_string()
            }),
            // A bridge mismatch could expose the listener to another slot's VPC.
            ("wrong bridge", |service| {
                service.service_nads[0].bridge = Some("br-sfc".to_string())
            }),
            // Server listeners must not request a client lease from CNI.
            ("IPAM enabled", |service| {
                service.service_nads[0].ipam = Some(true)
            }),
            // A VF would break the hidden-SF isolation and capacity contract.
            ("wrong resource", |service| {
                service.service_nads[0].resource_type = ServiceNADResourceType::Vf
            }),
            // A different listener MTU would disagree with the qualified service-pod attachment.
            ("wrong MTU", |service| {
                service.service_nads[0].mtu = Some(9000)
            }),
        ];

        for (description, mutate) in cases {
            // Corrupt one contract at a time so the rejection proves that boundary.
            let mut dhcp = dhcp_service(slots);
            mutate(&mut dhcp);
            let mut interfaces = Vec::new();
            let error = slots
                .apply(
                    &[hbn_service(hbn_interfaces.clone()), dhcp],
                    &mut interfaces,
                )
                .expect_err(description);

            // Pure validation must leave the caller's inventory unchanged.
            assert!(
                error
                    .to_string()
                    .contains("requires exactly one DHCP interface"),
                "{description}: {error}"
            );
            assert!(interfaces.is_empty(), "{description}");
        }
    }

    /// Verifies dependency and NAD ownership errors cannot produce partially applied slot topology.
    /// Namespace-wide NAD uniqueness protects listener references even across separate services.
    #[test]
    fn rejects_missing_duplicate_dhcp_service_and_cross_service_nad_collision() {
        type InventoryMutation = fn(&mut Vec<ServiceDefinition>);

        // Only the service-list mutation varies; HBN and DHCP start with complete slot inventory.
        let slots = ServiceVpcSlots::new(1).expect("valid slot count");
        let mut hbn_interfaces = Vec::new();
        slots.append_hbn_interfaces(&mut hbn_interfaces);
        let cases: &[(&str, InventoryMutation, &str)] = &[
            // Complete HBN inventory cannot compensate for the absent DHCP service.
            (
                "missing DHCP service",
                |services| services.retain(|service| service.name != DHCP_SERVER_SERVICE_NAME),
                "service-VPC slots require a DHCP service definition",
            ),
            // Two DHCP definitions cannot safely own the same generated listener resources.
            (
                "duplicate DHCP service",
                |services| services.push(services[1].clone()),
                "service-VPC slots require exactly one DHCP service definition",
            ),
            // A different service must not reuse the namespace-wide slot NAD name.
            (
                "cross-service NAD collision",
                |services| {
                    services.push(ServiceDefinition {
                        service_nads: services[1].service_nads.clone(),
                        ..ServiceDefinition::new("other", "repo", "chart", "1")
                    })
                },
                "service-VPC slot d_iface_svc_0 requires exactly one DHCP interface and an IPAM-disabled SF NAD on its bridge with MTU 1500",
            ),
        ];

        for (description, mutate, expected) in cases {
            // Break one ownership boundary while retaining valid surrounding dependencies.
            let mut services = vec![hbn_service(hbn_interfaces.clone()), dhcp_service(slots)];
            mutate(&mut services);
            let mut interfaces = Vec::new();
            let error = slots
                .apply(&services, &mut interfaces)
                .expect_err(description);

            // Assert the intended rejection and that validation has no topology side effects.
            assert!(
                matches!(error, DpfError::ConfigError(ref message) if message == *expected),
                "{description}: {error}"
            );
            assert!(interfaces.is_empty(), "{description}");
        }
    }

    #[test]
    fn accepts_zero_and_maximum_slots_but_rejects_excess() {
        assert!(ServiceVpcSlots::new(0).unwrap().is_empty());
        assert_eq!(
            {
                let mut interfaces = Vec::new();
                ServiceVpcSlots::new(MAX_HBN_SERVICE_INTERFACES as u32)
                    .unwrap()
                    .append_hbn_interfaces(&mut interfaces);
                interfaces.len()
            },
            MAX_HBN_SERVICE_INTERFACES
        );
        assert!(ServiceVpcSlots::new(33).is_err());
    }

    /// Verifies reserved slot identities cannot be reused by existing interfaces or host-facing chains.
    #[test]
    fn rejects_every_generated_name_collision() {
        let slots = ServiceVpcSlots::new(1).unwrap();
        for (description, mut interfaces) in [
            // A duplicate parent name would reconcile an existing interface into the slot patch.
            (
                "interface template",
                vec![physical_interface("service-vpc-slot-0", None)],
            ),
            // HBN netdev reuse would give two distinct topology endpoints the same identity.
            (
                "HBN interface",
                vec![physical_interface("existing", Some("iface_svc_0"))],
            ),
            // A slot bridge must not reuse a physical port's OVS name.
            (
                "physical OVS name",
                vec![physical_interface("br-svc-0", None)],
            ),
            // A DHCP slot listener must not also participate in a host-facing service chain.
            (
                "DHCP chain",
                vec![DpuServiceInterfaceTemplateDefinition {
                    chained_svc_if: Some(vec![(
                        DHCP_SERVER_SERVICE_NAME.to_string(),
                        "d_iface_svc_0".to_string(),
                    )]),
                    ..physical_interface("host", None)
                }],
            ),
        ] {
            // Supply valid services so a missing dependency cannot masquerade as collision rejection.
            let mut hbn_interfaces = interfaces
                .iter()
                .flat_map(|interface| interface.chained_svc_if.iter().flatten())
                .filter(|(service, _)| service == DOCA_HBN_SERVICE_NAME)
                .map(|(_, name)| ServiceInterface {
                    name: name.clone(),
                    network: DOCA_HBN_SERVICE_NETWORK.to_string(),
                })
                .collect();
            slots.append_hbn_interfaces(&mut hbn_interfaces);
            let error = slots
                .apply(
                    &[hbn_service(hbn_interfaces), dhcp_service(slots)],
                    &mut interfaces,
                )
                .expect_err(description);
            assert!(
                error.to_string().contains("conflicts with reserved"),
                "{description}: {error}"
            );
        }

        for (bridge, patch_port) in [
            // The generated br-sfc patch endpoint cannot reuse another patch's peer name.
            ("br-pf3", "p_brsfc_to_svc-slot-0"),
            // The slot's peer patch cannot also be an existing bridge.
            ("svc-slot-0", "p-pf3"),
            // The new slot bridge cannot reuse an existing patch port name.
            ("br-pf3", "br-svc-0"),
        ] {
            let mut interfaces = vec![DpuServiceInterfaceTemplateDefinition {
                name: "existing".to_string(),
                iface_type: DpuServiceInterfaceTemplateType::Patch(DpuServiceInterfacePatch {
                    peer_bridge: bridge.to_string(),
                    peer_patch_name: patch_port.to_string(),
                    peer_external_ids: None,
                }),
                pf_id: 0,
                vf_id: 0,
                chained_svc_if: None,
            }];

            // Valid service definitions ensure this fails on the OVS collision rather than missing HBN.
            let mut hbn_interfaces = Vec::new();
            slots.append_hbn_interfaces(&mut hbn_interfaces);
            let error = slots
                .apply(
                    &[hbn_service(hbn_interfaces), dhcp_service(slots)],
                    &mut interfaces,
                )
                .expect_err("reserved OVS names must reject an existing patch");
            assert!(
                error.to_string().contains("conflicts with reserved"),
                "bridge {bridge}, patch {patch_port}: {error}"
            );
        }
    }

    /// Verifies slot generation cannot exceed HBN's pinned interface limit, even with complete services.
    #[test]
    fn rejects_slots_exceeding_remaining_hbn_capacity() {
        // Existing chains consume all HBN capacity; complete service definitions isolate that limit.
        let slots = ServiceVpcSlots::new(1).expect("valid slot count");
        let mut interfaces = (0..MAX_HBN_SERVICE_INTERFACES)
            .map(|index| physical_interface(&format!("p{index}"), Some(&format!("hbn{index}"))))
            .collect::<Vec<_>>();
        let original = interfaces.clone();
        let mut hbn_interfaces = (0..MAX_HBN_SERVICE_INTERFACES)
            .map(|index| ServiceInterface {
                name: format!("hbn{index}"),
                network: DOCA_HBN_SERVICE_NETWORK.to_string(),
            })
            .collect();
        slots.append_hbn_interfaces(&mut hbn_interfaces);

        // Only the remaining-capacity rejection proves that no extra HBN SF can be generated.
        let error = slots
            .apply(
                &[hbn_service(hbn_interfaces), dhcp_service(slots)],
                &mut interfaces,
            )
            .expect_err("HBN inventory is full");
        assert!(matches!(error, DpfError::ConfigError(ref message)
                if message == "service-VPC slot count 1 exceeds the remaining HBN interface capacity of 0"));
        assert_eq!(interfaces, original);
    }

    /// Verifies HBN inventory comparison ignores ordering while preserving all required endpoints.
    #[test]
    fn validates_exact_hbn_inventory_without_requiring_order() {
        let slots = ServiceVpcSlots::new(1).unwrap();
        let interfaces = vec![physical_interface("p0", Some("p0_if"))];
        let mut expected = vec![ServiceInterface {
            name: "p0_if".to_string(),
            network: DOCA_HBN_SERVICE_NETWORK.to_string(),
        }];
        slots.append_hbn_interfaces(&mut expected);
        // Reverse the complete inventory to prove membership, rather than ordering, defines the contract.
        expected.reverse();

        let mut interfaces = interfaces;
        assert!(
            slots
                .apply(
                    &[hbn_service(expected), dhcp_service(slots)],
                    &mut interfaces
                )
                .is_ok()
        );
    }

    /// Verifies HBN definitions match every resolved endpoint so unrelated DHCP errors cannot mask a split inventory.
    #[test]
    fn rejects_missing_duplicate_extra_and_wrong_network_hbn_inventory() {
        // Start with a complete base-plus-slot HBN inventory; each case invalidates one contract.
        let slots = ServiceVpcSlots::new(1).unwrap();
        let interfaces = vec![physical_interface("p0", Some("p0_if"))];
        let mut expected = vec![ServiceInterface {
            name: "p0_if".to_string(),
            network: DOCA_HBN_SERVICE_NETWORK.to_string(),
        }];
        slots.append_hbn_interfaces(&mut expected);
        // Preserve both endpoints while disconnecting the slot from HBN's shared network.
        let mut wrong_network = expected.clone();
        wrong_network[1].network = "wrong".to_string();

        for (description, inventory) in [
            // Omitting a base endpoint leaves a resolved chain without its HBN interface.
            ("missing base interface", vec![expected[1].clone()]),
            // Omitting the slot endpoint leaves its fixed patch without an HBN peer.
            ("missing slot interface", vec![expected[0].clone()]),
            // A duplicate slot endpoint must not consume a second SF under the same identity.
            (
                "duplicate interface",
                vec![
                    expected[0].clone(),
                    expected[1].clone(),
                    expected[1].clone(),
                ],
            ),
            // An extra endpoint has no resolved chain even though it names HBN's valid network.
            (
                "extra interface",
                vec![
                    expected[0].clone(),
                    expected[1].clone(),
                    ServiceInterface {
                        name: "extra".to_string(),
                        network: DOCA_HBN_SERVICE_NETWORK.to_string(),
                    },
                ],
            ),
            // Matching names alone cannot prove the slot uses the intended SF network.
            ("wrong network", wrong_network),
        ] {
            // Complete DHCP setup prevents its independent validation from hiding an HBN regression.
            let mut interfaces = interfaces.clone();
            let error = slots
                .apply(
                    &[hbn_service(inventory), dhcp_service(slots)],
                    &mut interfaces,
                )
                .expect_err(description);

            // Reject specifically for HBN mismatch before adding any slot topology.
            assert!(
                matches!(&error, DpfError::ConfigError(message)
                    if message == "doca-hbn interface inventory must exactly match the resolved DPF HBN chains"),
                "{description}: {error}"
            );
            assert_eq!(interfaces, vec![physical_interface("p0", Some("p0_if"))]);
        }
    }

    /// Verifies every HBN chain endpoint needs a configured netdev, even when chains share one parent.
    #[test]
    fn rejects_an_unconfigured_second_hbn_chain_on_one_interface() {
        // Add an extra chain endpoint while leaving the service's base-plus-slot inventory complete.
        let slots = ServiceVpcSlots::new(1).unwrap();
        let mut interface = physical_interface("p0", Some("p0_if"));
        interface
            .chained_svc_if
            .as_mut()
            .expect("physical interface fixture has one HBN chain")
            .push((DOCA_HBN_SERVICE_NAME.to_string(), "p0_if_2".to_string()));
        let mut configured_hbn_interfaces = vec![ServiceInterface {
            name: "p0_if".to_string(),
            network: DOCA_HBN_SERVICE_NETWORK.to_string(),
        }];
        slots.append_hbn_interfaces(&mut configured_hbn_interfaces);

        // Valid DHCP makes the unconfigured HBN endpoint the only rejection reason.
        let error = slots
            .apply(
                &[hbn_service(configured_hbn_interfaces), dhcp_service(slots)],
                &mut vec![interface],
            )
            .expect_err("unconfigured second HBN chain");
        assert!(
            matches!(&error, DpfError::ConfigError(message)
                if message == "doca-hbn interface inventory must exactly match the resolved DPF HBN chains"),
            "{error}"
        );
    }

    /// Verifies slots require exactly one HBN service so missing or ambiguous ownership cannot provision topology.
    #[test]
    fn rejects_missing_or_duplicate_hbn_service() {
        // Supply valid DHCP while omitting only the required HBN definition.
        let slots = ServiceVpcSlots::new(1).unwrap();
        let interfaces = vec![physical_interface("p0", Some("p0_if"))];
        let mut no_service_interfaces = interfaces.clone();
        let error = slots
            .apply(&[dhcp_service(slots)], &mut no_service_interfaces)
            .expect_err("missing HBN service");
        assert!(
            matches!(&error, DpfError::ConfigError(message)
                if message == "service-VPC slots require a doca-hbn service definition"),
            "{error}"
        );
        assert_eq!(no_service_interfaces, interfaces);

        // Duplicate an otherwise complete HBN definition without invalidating DHCP.
        let mut hbn_interfaces = vec![ServiceInterface {
            name: "p0_if".to_string(),
            network: DOCA_HBN_SERVICE_NETWORK.to_string(),
        }];
        slots.append_hbn_interfaces(&mut hbn_interfaces);
        let hbn = hbn_service(hbn_interfaces);
        let mut duplicate_service_interfaces = interfaces.clone();
        let error = slots
            .apply(
                &[hbn.clone(), hbn, dhcp_service(slots)],
                &mut duplicate_service_interfaces,
            )
            .expect_err("duplicate HBN service");
        assert!(
            matches!(&error, DpfError::ConfigError(message)
                if message == "service-VPC slots require exactly one doca-hbn service definition"),
            "{error}"
        );
        assert_eq!(duplicate_service_interfaces, interfaces);
    }
}
