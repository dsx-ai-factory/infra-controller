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
//! Each configured slot `N` reserves the otherwise empty OVS bridge `br-svc-N`
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
//! to the service interface.

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::error::DpfError;
use crate::types::{
    DOCA_HBN_SERVICE_NAME, DOCA_HBN_SERVICE_NETWORK, DpuServiceInterfacePatch,
    DpuServiceInterfaceTemplateDefinition, DpuServiceInterfaceTemplateType, ServiceDefinition,
    ServiceInterface,
};

pub(crate) const MAX_HBN_SERVICE_INTERFACES: usize = 32;

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

    /// Appends the dedicated OVS bridge for every configured slot.
    pub(crate) fn append_ovs_bridges(self, script: &mut String) {
        // Reprovision performs a full OS installation, so OVSDB is reset before this script runs;
        // bridges from a previous, larger slot count do not need explicit deletion here.
        for slot in self.slots() {
            // Each slot bridge is dedicated to one HBN patch and one consumer service patch.
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

    #[test]
    fn generates_complete_slot_topology() {
        let slots = ServiceVpcSlots::new(2).unwrap();
        let mut interfaces = Vec::new();
        let mut hbn_interfaces = Vec::new();
        slots.append_hbn_interfaces(&mut hbn_interfaces);
        slots
            .apply(&[hbn_service(hbn_interfaces.clone())], &mut interfaces)
            .unwrap();
        let mut ovs = String::new();
        slots.append_ovs_bridges(&mut ovs);

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

    #[test]
    fn generated_slot_participates_in_sf_capacity() {
        let slots = ServiceVpcSlots::new(1).unwrap();
        let mut hbn_interfaces = Vec::new();
        slots.append_hbn_interfaces(&mut hbn_interfaces);
        let mut interfaces = Vec::new();
        slots
            .apply(&[hbn_service(hbn_interfaces)], &mut interfaces)
            .unwrap();

        assert!(crate::calculate_pf_total_sf(&interfaces, None, 0, 0).is_err());
        assert_eq!(
            crate::calculate_pf_total_sf(&interfaces, None, 1, 0).unwrap(),
            1
        );
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

    #[test]
    fn rejects_every_generated_name_collision() {
        let slots = ServiceVpcSlots::new(1).unwrap();
        for (description, mut interfaces) in [
            (
                "interface template",
                vec![physical_interface("service-vpc-slot-0", None)],
            ),
            (
                "HBN interface",
                vec![physical_interface("existing", Some("iface_svc_0"))],
            ),
            (
                "physical OVS name",
                vec![physical_interface("br-svc-0", None)],
            ),
        ] {
            assert!(
                slots.apply(&[], &mut interfaces).is_err(),
                "accepted colliding {description}"
            );
        }

        for (bridge, patch_port) in [
            ("br-pf3", "p_brsfc_to_svc-slot-0"),
            ("svc-slot-0", "p-pf3"),
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

            assert!(
                slots.apply(&[], &mut interfaces).is_err(),
                "accepted colliding bridge {bridge} and patch port {patch_port}"
            );
        }
    }

    #[test]
    fn rejects_slots_exceeding_remaining_hbn_capacity() {
        let mut interfaces = (0..MAX_HBN_SERVICE_INTERFACES)
            .map(|index| physical_interface(&format!("p{index}"), Some(&format!("hbn{index}"))))
            .collect::<Vec<_>>();

        assert!(
            ServiceVpcSlots::new(1)
                .unwrap()
                .apply(&[], &mut interfaces)
                .is_err()
        );
    }

    #[test]
    fn validates_exact_hbn_inventory_without_requiring_order() {
        let slots = ServiceVpcSlots::new(1).unwrap();
        let interfaces = vec![physical_interface("p0", Some("p0_if"))];
        let mut expected = vec![ServiceInterface {
            name: "p0_if".to_string(),
            network: DOCA_HBN_SERVICE_NETWORK.to_string(),
        }];
        slots.append_hbn_interfaces(&mut expected);
        expected.reverse();

        let mut interfaces = interfaces;
        assert!(
            slots
                .apply(&[hbn_service(expected)], &mut interfaces)
                .is_ok()
        );
    }

    #[test]
    fn rejects_missing_duplicate_extra_and_wrong_network_hbn_inventory() {
        let slots = ServiceVpcSlots::new(1).unwrap();
        let interfaces = vec![physical_interface("p0", Some("p0_if"))];
        let mut expected = vec![ServiceInterface {
            name: "p0_if".to_string(),
            network: DOCA_HBN_SERVICE_NETWORK.to_string(),
        }];
        slots.append_hbn_interfaces(&mut expected);
        let mut wrong_network = expected.clone();
        wrong_network[1].network = "wrong".to_string();

        for (description, inventory) in [
            ("missing base interface", vec![expected[1].clone()]),
            ("missing slot interface", vec![expected[0].clone()]),
            (
                "duplicate interface",
                vec![
                    expected[0].clone(),
                    expected[1].clone(),
                    expected[1].clone(),
                ],
            ),
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
            ("wrong network", wrong_network),
        ] {
            let mut interfaces = interfaces.clone();
            assert!(
                slots
                    .apply(&[hbn_service(inventory)], &mut interfaces)
                    .is_err(),
                "accepted {description}"
            );
            assert_eq!(interfaces, vec![physical_interface("p0", Some("p0_if"))]);
        }
    }

    #[test]
    fn rejects_an_unconfigured_second_hbn_chain_on_one_interface() {
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

        assert!(
            slots
                .apply(
                    &[hbn_service(configured_hbn_interfaces)],
                    &mut vec![interface],
                )
                .is_err()
        );
    }

    #[test]
    fn rejects_missing_or_duplicate_hbn_service() {
        let slots = ServiceVpcSlots::new(1).unwrap();
        let interfaces = vec![physical_interface("p0", Some("p0_if"))];
        let mut no_service_interfaces = interfaces.clone();
        assert!(slots.apply(&[], &mut no_service_interfaces).is_err());
        assert_eq!(no_service_interfaces, interfaces);

        let mut hbn_interfaces = vec![ServiceInterface {
            name: "p0_if".to_string(),
            network: DOCA_HBN_SERVICE_NETWORK.to_string(),
        }];
        slots.append_hbn_interfaces(&mut hbn_interfaces);
        let hbn = hbn_service(hbn_interfaces);
        let mut duplicate_service_interfaces = interfaces.clone();
        assert!(
            slots
                .apply(&[hbn.clone(), hbn], &mut duplicate_service_interfaces)
                .is_err()
        );
        assert_eq!(duplicate_service_interfaces, interfaces);
    }
}
