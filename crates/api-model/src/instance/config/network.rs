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

use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::net::IpAddr;

use carbide_uuid::machine::DpuMachineId;
use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ConfigValidationError;

// Specifies whether a network interface is physical network function (PF)
// or a virtual network function
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterfaceFunctionType {
    Physical = 0,
    Virtual = 1,
}

/// Uniquely identifies an interface on the instance
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(tag = "type")]
pub enum InterfaceFunctionId {
    #[serde(rename = "physical")]
    Physical {
        // This might later on also contain the DPU ID
    },
    #[serde(rename = "virtual")]
    Virtual {
        /// Uniquely identifies the VF on a DPU
        ///
        /// The first VF assigned to a host must use ID 1.
        /// All other IDs need to be consecutively assigned.
        id: u8,
        // This might later on also contain the DPU ID
    },
}

impl InterfaceFunctionId {
    /// Returns an iterator that yields all valid InterfaceFunctionIds
    ///
    /// The first returned item is the `Physical`.
    /// Then the list of `Virtual`s will follow
    pub fn iter_all() -> impl Iterator<Item = InterfaceFunctionId> {
        (-1..=INTERFACE_VFID_MAX as i32).map(|idx| {
            if idx == -1 {
                InterfaceFunctionId::Physical {}
            } else {
                InterfaceFunctionId::Virtual { id: idx as u8 }
            }
        })
    }

    /// Returns whether ID refers to a physical or virtual function
    pub fn function_type(&self) -> InterfaceFunctionType {
        match self {
            InterfaceFunctionId::Physical { .. } => InterfaceFunctionType::Physical,
            InterfaceFunctionId::Virtual { .. } => InterfaceFunctionType::Virtual,
        }
    }

    /// Tries to convert a numeric identifier that represents a virtual function
    /// into a `InterfaceFunctionId::Virtual`.
    /// This will return an error if the ID is not in the valid range.
    pub fn try_virtual_from(id: u8) -> Result<InterfaceFunctionId, InvalidVirtualFunctionId> {
        if !(INTERFACE_VFID_MIN..=INTERFACE_VFID_MAX).contains(&id) {
            return Err(InvalidVirtualFunctionId());
        }

        Ok(InterfaceFunctionId::Virtual { id })
    }
}

/// An ID is not a valid virtual function ID due to being out of bounds
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct InvalidVirtualFunctionId();

/// Desired network configuration for an instance
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceNetworkConfig {
    /// Configures how instance network interfaces are set up.
    /// Mutually exclusive with `auto`: when `auto` is true, this
    /// MUST be empty, When `auto` is false, this lists the explicit
    /// interface configuration the caller wants applied.
    pub interfaces: Vec<InstanceInterfaceConfig>,

    /// When true, NICO (or potentially some pluggable SDN backend) will
    /// auto-resolve the instance's network interfaces from the host's
    /// HostInband network segments. Only valid for instances on zero-DPU
    /// hosts (well, no DPU, *or* DPU in NIC mode).
    ///
    /// It is also important to note that on the wire (request AND response),
    /// `auto_config: {...}` only travels with `interfaces: []`, but internally some
    /// other things are happening.
    ///
    /// On allocation/update, NICo resolves the empty interfaces: [] into
    /// one entry per HostInband segment on the host, then stores the
    /// fully-resolved config internally (allowing storage, status, IP
    /// bookkeeping, config diffs, etc to all operate on real interfaces).
    ///
    /// Then, at the model <-> RPC boundary, the resolved interfaces are
    /// stripped off to `[]`, so callers reading the instance config back
    /// simply see what they originally sent (`auto: {...}` with no interfaces).
    ///
    /// The resolved per-interface details (IP, MAC, gateway, prefix) appear in
    /// `Instance.status.network.interfaces` like usual.
    pub auto_config: Option<InstanceNetworkAutoConfig>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceNetworkAutoConfig {
    /// The logical VPC to allocate into when `auto` networking is used.
    ///
    /// Auto networking resolves the concrete HostInband segment from the host,
    /// so the segment itself does not have to carry VPC ownership.
    pub vpc_id: VpcId,
}

/// Struct to store instance network config updated request with current config.
/// Current config is kept here to release these resources once instance moves to the new network
/// resources.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceNetworkConfigUpdate {
    // Current configuration which will be deallocated.
    // If any interface is present in requested config with same network details and function id,
    // that should be removed from the old config and must not be deallocated.
    pub old_config: InstanceNetworkConfig,

    // New requested config.
    pub new_config: InstanceNetworkConfig,
}

impl InstanceNetworkConfig {
    /// Returns a network configuration for a single physical interface
    pub fn for_segment_ids(
        network_segment_ids: &[NetworkSegmentId],
        device_locators: &[DeviceLocator],
        vpc_ids: &[VpcId],
    ) -> Self {
        if device_locators.is_empty() {
            Self {
                interfaces: vec![InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Physical {},
                    network_segment_id: network_segment_ids.first().copied(),
                    network_details: Some(NetworkDetails::NetworkSegment(
                        network_segment_ids.first().copied().unwrap(),
                    )),
                    vpc_selection: None,
                    ip_addrs: HashMap::default(),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::default(),
                    network_segment_gateways: HashMap::default(),
                    host_inband_mac_address: None,
                    device_locator: None,
                    internal_uuid: uuid::Uuid::nil(),
                    vpc_id: vpc_ids.first().copied(),
                }],
                auto_config: None,
            }
        } else {
            Self {
                interfaces: device_locators
                    .iter()
                    .enumerate()
                    .map(|(dl_index, dl)| InstanceInterfaceConfig {
                        function_id: InterfaceFunctionId::Physical {},
                        network_segment_id: network_segment_ids.get(dl_index).copied(),
                        network_details: Some(NetworkDetails::NetworkSegment(
                            network_segment_ids[dl_index],
                        )),
                        vpc_selection: None,
                        ip_addrs: HashMap::default(),
                        requested_ip_addr: None,
                        ipv6_interface_config: None,
                        routing_profile: None,
                        interface_prefixes: HashMap::default(),
                        network_segment_gateways: HashMap::default(),
                        host_inband_mac_address: None,
                        device_locator: Some(dl.clone()),
                        internal_uuid: uuid::Uuid::nil(),
                        vpc_id: vpc_ids.get(dl_index).copied(),
                    })
                    .collect(),
                auto_config: None,
            }
        }
    }

    /// Returns a network configuration for a single physical interface
    pub fn for_vpc_prefix_id(vpc_prefix_id: VpcPrefixId, vpc_id: Option<VpcId>) -> Self {
        Self {
            interfaces: vec![InstanceInterfaceConfig {
                function_id: InterfaceFunctionId::Physical {},
                network_segment_id: None,
                network_details: Some(NetworkDetails::VpcPrefixId(vpc_prefix_id)),
                vpc_selection: None,
                ip_addrs: HashMap::default(),
                requested_ip_addr: None,
                ipv6_interface_config: None,
                routing_profile: None,
                interface_prefixes: HashMap::default(),
                network_segment_gateways: HashMap::default(),
                host_inband_mac_address: None,
                device_locator: None,
                internal_uuid: uuid::Uuid::nil(),
                vpc_id,
            }],
            auto_config: None,
        }
    }

    /// Returns this config as it should appear on the wire: for `auto`
    /// configs, the resolved interfaces are stripped so external callers see
    /// just their request (`{ auto: true, interfaces: [] }`). The fully-
    /// resolved interfaces still drive `InstanceNetworkStatus` population
    /// from the internal model. For non-auto configs, returns `self`
    /// unchanged.
    ///
    /// This exists to keep the input config from the user represented
    /// back to them as they sent it, and mask any internal interface
    /// resolution that happened as a result of `auto`.
    pub fn into_external_view(self) -> Self {
        if self.auto_config.is_some() {
            Self {
                interfaces: vec![],
                auto_config: self.auto_config,
            }
        } else {
            self
        }
    }

    /// Returns the DPU machine IDs used by the instance network configuration.
    pub fn get_used_dpus(
        &self,
        device_to_id_map: &HashMap<String, Vec<DpuMachineId>>,
        primary_dpu_machine_id: Option<DpuMachineId>,
    ) -> Vec<DpuMachineId> {
        let device_locators: Vec<&DeviceLocator> = self
            .interfaces
            .iter()
            .filter_map(|i| i.device_locator.as_ref())
            .collect();

        let legacy_physical_interface_count = self
            .interfaces
            .iter()
            .filter(|iface| {
                iface.function_id == InterfaceFunctionId::Physical {}
                    && iface.device_locator.is_none()
            })
            .count();

        let use_primary_dpu_only = legacy_physical_interface_count > 0
            || device_locators.is_empty()
            || device_to_id_map.is_empty();

        if use_primary_dpu_only {
            return primary_dpu_machine_id.into_iter().collect();
        }

        let used_dpus: Vec<DpuMachineId> = device_locators
            .iter()
            .filter_map(|device_locator| {
                device_to_id_map
                    .get(&device_locator.device)
                    .and_then(|dpu_ids| dpu_ids.get(device_locator.device_instance))
                    .copied()
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        if used_dpus.is_empty() {
            return device_to_id_map
                .values()
                .flatten()
                .copied()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
        }

        used_dpus
    }

    /// Validates the network configuration.
    ///
    /// Note: this is also called on POST-resolution configs (i.e. after
    /// `add_inband_interfaces_to_config` has expanded an `auto` request into
    /// underlying interfaces), so it must not reject the combination
    /// `auto: true` + non-empty interfaces here. The "auto must arrive with
    /// empty interfaces" rule is enforced in RPC <-> model conversion, which
    /// only runs on user input. A resolved `auto` interface is identified by
    /// its HostInband segment (one bare `Physical {}` per segment), so those
    /// configs are validated against that rule rather than the per-device
    /// function-id bucketing.
    pub fn validate(&self, allow_instance_vf: bool) -> Result<(), ConfigValidationError> {
        if !allow_instance_vf
            && self
                .interfaces
                .iter()
                .any(|i| matches!(i.function_id, InterfaceFunctionId::Virtual { .. }))
        {
            return Err(ConfigValidationError::InvalidValue(
                "Virtual functions are disabled by site configuration".to_string(),
            ));
        }

        if self.auto_config.is_some() {
            // Resolved `auto` interfaces are system-injected by
            // `add_inband_interfaces_to_config`: one bare `Physical {}` per
            // HostInband segment on the host, with no `device_locator` (a
            // zero-DPU host has no DPU to locate). Their identity is the
            // network segment itself -- enforced by the uniqueness check
            // below -- so the per-device function-id bucketing doesn't apply
            // here; it would read every injected PF beyond the first as a
            // duplicate of one device.
            if let Some(iface) = self.interfaces.iter().find(|iface| {
                !matches!(iface.function_id, InterfaceFunctionId::Physical {})
                    || iface.device_locator.is_some()
            }) {
                return Err(ConfigValidationError::InvalidValue(format!(
                    "auto network configs hold only system-resolved physical host-inband \
                     interfaces; found function {:?} with device locator {:?}",
                    iface.function_id, iface.device_locator,
                )));
            }
        } else {
            validate_interface_function_ids(
                &self.interfaces,
                |iface| &iface.function_id,
                |iface| iface.device_locator.as_ref(),
            )
            .map_err(ConfigValidationError::InvalidValue)?;
        }

        // Note: We can't fully validate the network segment IDs here
        // We validate that the ID is not duplicated, but not whether it actually exists
        // or belongs to the tenant. This validation is currently happening in the
        // cloud API, and when we try to allocate IPs.
        //
        // Multiple interfaces currently can't reference the same segment ID due to
        // how DHCP works. It would be ambiguous during a DHCP request which
        // interface it references, since the interface is resolved by the CircuitId
        // and thereby by the network segment ID
        let mut used_segment_ids = HashSet::new();
        for iface in self.interfaces.iter() {
            let Some(network_segment_id) = &iface.network_segment_id else {
                return Err(ConfigValidationError::MissingSegment(
                    iface.function_id.clone(),
                ));
            };

            if !used_segment_ids.insert(network_segment_id) {
                return Err(ConfigValidationError::InvalidValue(format!(
                    "Multiple network interfaces use the same network segment {network_segment_id}"
                )));
            }

            // Every concrete address must retain its interface prefix. An IPv6
            // interface prefix may intentionally have no address when the
            // owning VPC uses SLAAC; IPv4 retains the one-to-one invariant.
            //
            // TODO(chet): Only do this if there are actual prefixes set for
            // this interface. If there aren't, its because this is an old
            // instance which existed prior to introducing instance_prefixes.
            // Once all instances are configured with prefixes, then there's
            // no need for an empty check.
            if !iface.interface_prefixes.is_empty()
                && (iface
                    .ip_addrs
                    .keys()
                    .any(|prefix_id| !iface.interface_prefixes.contains_key(prefix_id))
                    || iface.interface_prefixes.iter().any(|(prefix_id, prefix)| {
                        !iface.ip_addrs.contains_key(prefix_id) && prefix.is_ipv4()
                    }))
            {
                return Err(ConfigValidationError::NetworkPrefixAllocationMismatch);
            }
        }

        Ok(())
    }

    pub fn verify_update_allowed_to(
        &self,
        _new_config: &Self,
    ) -> Result<(), ConfigValidationError> {
        Ok(())
    }

    pub fn is_network_config_update_requested(&self, new_config: &Self) -> bool {
        // Remove all service-generated properties before validating the config
        let mut current = self.clone();
        let mut new_config = new_config.clone();
        for iface in &mut current.interfaces {
            iface.ip_addrs.clear();
            iface.interface_prefixes.clear();
            iface.network_segment_gateways.clear();
            iface.host_inband_mac_address = None;
            iface.internal_uuid = uuid::Uuid::nil();
            iface.vpc_id = None;

            // Automatic intent is compared independently of its generated
            // prefix, segment, and any legacy explicit-IP representation.
            if iface.vpc_selection.is_some() {
                iface.network_details = None;
                iface.requested_ip_addr = None;
                iface.ipv6_interface_config = None;
            }

            // It is possible that cloud sends network_segment_id with network_details as well.
            if iface.network_details.is_some() || iface.vpc_selection.is_some() {
                iface.network_segment_id = None;
            }
        }

        for iface in &mut new_config.interfaces {
            // A resolved automatic selection may be resubmitted from an
            // internal caller; compare only its VPC and family intent.
            if iface.vpc_selection.is_some() {
                iface.network_details = None;
                iface.requested_ip_addr = None;
                iface.ipv6_interface_config = None;
                iface.ip_addrs.clear();
                iface.interface_prefixes.clear();
                iface.network_segment_gateways.clear();
                iface.host_inband_mac_address = None;
            }

            // It is possible that cloud sends network_segment_id with network_details as well.
            if iface.network_details.is_some() || iface.vpc_selection.is_some() {
                iface.network_segment_id = None;
            }
            iface.internal_uuid = uuid::Uuid::nil();
            iface.vpc_id = None;
        }

        current != new_config
    }

    // This function copies exiting resources which are unchanged in new network config.
    // This usually represents the case of adding/deleting a VF.
    // This function also returns the copied resources so that state machine can filter out used
    // resources and release other resources.
    // The algorithm should remain same for copying and filtering to keep things consistent.
    pub fn copy_existing_resources<'a>(
        &mut self,
        current_config: &'a Self,
    ) -> Vec<&'a InstanceInterfaceConfig> {
        let mut common_function_ids = Vec::new();

        // Virtual function id does not change during the instance life cycle.
        // If a VF is deleted, cloud won't send that id to carbide.
        // e.g. VF configured 0,1,2,3; tenant deletes vf id 2. In this case cloud will forward new
        // config only with vf id as 0,1,3.
        for interface in &mut self.interfaces {
            let existing_interface = current_config.interfaces.iter().find(|x| {
                // An unresolved request may inherit the active resolution. Once
                // both sides are resolved, prefix and segment identity must match
                // so cleanup does not classify distinct resources as common.
                let requested_resolution_matches = match interface.resolved_vpc_prefixes() {
                    None => true,
                    Some(requested_resolution) => {
                        x.resolved_vpc_prefixes() == Some(requested_resolution)
                            && x.generated_network_segment_id()
                                == interface.generated_network_segment_id()
                    }
                };

                let is_network_same = match (&interface.vpc_selection, &x.vpc_selection) {
                    (Some(requested), Some(existing)) => {
                        requested == existing && requested_resolution_matches
                    }
                    (Some(requested), None) => {
                        // Explicit-prefix intent may become automatic intent without
                        // replacing resources when its resolved VPC and families match.
                        // `requested_resolution_matches` accepts unresolved intent, so require
                        // the exact family shape before copied addresses suppress allocation.
                        x.vpc_id == Some(requested.vpc_id)
                            && x.resolved_vpc_prefixes().is_some_and(|resolved| {
                                match requested.family_mode {
                                    InstanceInterfaceIpFamilyMode::Ipv4Only => {
                                        resolved.ipv4_vpc_prefix_id.is_some()
                                            && resolved.ipv6_vpc_prefix_id.is_none()
                                    }
                                    InstanceInterfaceIpFamilyMode::Ipv6Only => {
                                        resolved.ipv4_vpc_prefix_id.is_none()
                                            && resolved.ipv6_vpc_prefix_id.is_some()
                                    }
                                    InstanceInterfaceIpFamilyMode::DualStack => {
                                        resolved.ipv4_vpc_prefix_id.is_some()
                                            && resolved.ipv6_vpc_prefix_id.is_some()
                                    }
                                }
                            })
                            && requested_resolution_matches
                    }
                    _ if interface.network_details.is_some() => {
                        // TODO: Compare requested_ip_addr once explicit address replacement is
                        // safe. Matching VPC-prefix intent currently reuses the active address;
                        // a replacement in the same linknet can violate PostgreSQL's network-prefix
                        // overlap constraint because allocation precedes old-resource cleanup.
                        x.network_details == interface.network_details
                            && x.ipv6_interface_config == interface.ipv6_interface_config
                    }
                    _ => {
                        interface.network_segment_id.is_some()
                            && x.network_segment_id == interface.network_segment_id
                    }
                };

                if is_network_same {
                    // Exactly same interface id and device locator must be used.
                    interface.function_id == x.function_id
                        && interface.device_locator == x.device_locator
                } else {
                    false
                }
            });

            if let Some(existing_interface) = existing_interface {
                // Copy all allocated resources.
                interface.ip_addrs = existing_interface.ip_addrs.clone();
                interface.interface_prefixes = existing_interface.interface_prefixes.clone();
                interface.network_segment_gateways =
                    existing_interface.network_segment_gateways.clone();
                interface.host_inband_mac_address = existing_interface.host_inband_mac_address;

                if interface.vpc_selection.is_some() {
                    // Automatic intent reuses the resolution without reviving
                    // explicit address intent from an earlier configuration.
                    interface.network_details = existing_interface.network_details.clone();
                    interface.requested_ip_addr = None;
                    interface.ipv6_interface_config = existing_interface
                        .ipv6_interface_config
                        .as_ref()
                        .map(|ipv6| Ipv6InterfaceConfig {
                            vpc_prefix_id: ipv6.vpc_prefix_id,
                            requested_ip_addr: None,
                        });
                } else {
                    interface.requested_ip_addr = existing_interface.requested_ip_addr;
                    interface.ipv6_interface_config =
                        existing_interface.ipv6_interface_config.clone();
                }

                if interface.network_details.is_some() {
                    interface.network_segment_id = existing_interface.network_segment_id;
                }
                interface.vpc_id = existing_interface.vpc_id;
                common_function_ids.push(existing_interface);
            }
        }

        common_function_ids
    }

    /// Returns true if all interfaces on this instance are equivalent to the host's in-band
    /// interface, meaning they belong to a network segment of type
    /// [`NetworkSegmentType::HostInband`]. This is in contrast to DPU-based interfaces where the
    /// instance sees an overlay network.
    pub fn is_host_inband(&self) -> bool {
        self.interfaces.iter().all(|i| i.is_host_inband())
    }

    /// `uses_operator_managed_networking` returns true when every configured interface uses the
    /// operator's HostInband network instead of the NICo DPU data plane. The config must be
    /// nonempty because `is_host_inband` is vacuously true for an empty interface list.
    pub fn uses_operator_managed_networking(&self) -> bool {
        !self.interfaces.is_empty() && self.is_host_inband()
    }
}

/// Validates that any container which has elements that have InterfaceFunctionIds
/// assigned assigned is using unique and valid FunctionIds.
pub fn validate_interface_function_ids<
    T,
    F: Fn(&T) -> &InterfaceFunctionId,
    G: Fn(&T) -> Option<&DeviceLocator>,
>(
    container: &[T],
    get_function_id: F,
    get_device_locator: G,
) -> Result<(), String> {
    if container.is_empty() {
        // Empty interfaces can be filled via host's host_inband interfaces later. If it's still
        // empty then, we throw an error later.
        return Ok(());
    }

    // We need 1 physical interface, virtual interfaces must start at VFID 0,
    // and IDs must not be duplicated
    let mut used_functions: HashMap<Option<&DeviceLocator>, Vec<i32>> = HashMap::new();

    for (idx, iface) in container.iter().enumerate() {
        let function_id = get_function_id(iface);
        let device_locator = get_device_locator(iface);

        if let InterfaceFunctionId::Virtual { id } = function_id
            && !(INTERFACE_VFID_MIN..=INTERFACE_VFID_MAX).contains(id)
        {
            return Err(format!(
                "Invalid interface virtual function ID {id} for network interface at index {idx}"
            ));
        }

        let func_id = match function_id {
            InterfaceFunctionId::Physical {} => -1,
            InterfaceFunctionId::Virtual { id } => (*id) as i32,
        };

        used_functions
            .entry(device_locator)
            .or_default()
            .push(func_id);

        // Note: We can't validate the network segment ID here
    }

    // Now there can be a gap in virtual id. We can only validate that if physical id is given or
    // not.
    for (device_locator, fids) in &mut used_functions {
        fids.sort();
        if let Some(pf) = fids.first() {
            if *pf != -1 {
                return Err(format!(
                    "Missing Physical Function for device {}",
                    device_locator.cloned().unwrap_or_default(),
                ));
            }
        } else {
            return Err(format!(
                "No Function is given for device {}",
                device_locator.cloned().unwrap_or_default(),
            ));
        };

        let fids_hash: HashSet<i32> = HashSet::from_iter(fids.iter().copied());
        if fids.len() != fids_hash.len() {
            // Duplicate function ids are present.
            return Err(format!(
                "Duplicate fucntion ids are present for device {}: {:?}",
                device_locator.cloned().unwrap_or_default(),
                fids
            ));
        }
    }

    Ok(())
}

/// Address families requested for automatic VPC prefix and address selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceInterfaceIpFamilyMode {
    /// Allocate one IPv4 prefix and interface address.
    Ipv4Only,
    /// Allocate one IPv6 prefix and interface address.
    Ipv6Only,
    /// Allocate one prefix and interface address from each family.
    DualStack,
}

/// Caller intent for automatic prefix and address selection from one VPC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInterfaceVpcSelection {
    /// The single logical VPC from which prefixes must be selected.
    pub vpc_id: VpcId,
    /// The exact address families Core must allocate.
    pub family_mode: InstanceInterfaceIpFamilyMode,
}

/// VPC prefixes resolved for an instance interface, keyed by address family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInterfaceResolvedVpcPrefixes {
    /// The selected IPv4 parent prefix, when IPv4 was requested.
    pub ipv4_vpc_prefix_id: Option<VpcPrefixId>,
    /// The selected IPv6 parent prefix, when IPv6 was requested.
    pub ipv6_vpc_prefix_id: Option<VpcPrefixId>,
}

/// Enum to keep either network segment id or vpc_prefix id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkDetails {
    NetworkSegment(NetworkSegmentId),
    VpcPrefixId(VpcPrefixId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash, Default)]
pub struct DeviceLocator {
    pub device: String,
    pub device_instance: usize,
}
impl Display for DeviceLocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.device, self.device_instance)
    }
}

/// IPv6 dual-stack configuration for an instance interface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ipv6InterfaceConfig {
    pub vpc_prefix_id: VpcPrefixId,
    pub requested_ip_addr: Option<std::net::Ipv6Addr>,
}

/// Routing-profile options that can be narrowed for an instance interface.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInterfaceRoutingProfile {
    /// Prefixes this interface is allowed to announce as anycast routes.
    #[serde(default)]
    pub allowed_anycast_prefixes: Vec<IpNetwork>,
}

/// The configuration that a customer desires for an instances network interface
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInterfaceConfig {
    /// Uniquely identifies the interface on the instance
    pub function_id: InterfaceFunctionId,
    /// Tenant can provide vpc_prefix_id instead of network segment id.
    /// In case of vpc_prefix_id, carbide should allocate a new network segment and use it for
    /// further IP allocation.
    pub network_details: Option<NetworkDetails>,

    /// Caller intent for automatic selection from a VPC.
    ///
    /// Resolved prefix IDs remain in the legacy-readable explicit fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vpc_selection: Option<InstanceInterfaceVpcSelection>,

    /// The network segment this interface is attached to.
    /// In case vpc_prefix_id is provided, a new segment has to be created and assign here.
    pub network_segment_id: Option<NetworkSegmentId>,
    /// The IP address we allocated for each network prefix for this interface
    /// This is not populated if we have not allocated IP addresses yet.
    #[serde(
        default,
        deserialize_with = "deserialize_network_prefix_id_ipaddr_map",
        serialize_with = "serialize_network_prefix_id_ipaddr_map"
    )]
    pub ip_addrs: HashMap<NetworkPrefixId, IpAddr>,

    /// IP address allocation that was explicitly requested by a caller from the VPC prefix of the interface.
    pub requested_ip_addr: Option<IpAddr>,

    /// Optional IPv6 dual-stack configuration. When set alongside a
    /// VpcPrefixId in network_details, both prefixes are allocated to a single segment.
    #[serde(rename = "ipv6")]
    pub ipv6_interface_config: Option<Ipv6InterfaceConfig>,

    /// Optional routing-profile settings that narrow the owning VPC profile for this interface.
    #[serde(default)]
    pub routing_profile: Option<InstanceInterfaceRoutingProfile>,

    /// The interface-specific prefix allocation we carved out from each
    /// network prefix for this interface (e.g. in FNN we might carve out
    /// a /31 for an interface, whereas in ETV we just allocate a /32).
    ///
    /// Every concrete address in `ip_addrs` has a corresponding entry here
    /// (even if it is only the host prefix derived from that address). Model
    /// validation permits an IPv6 prefix without an `ip_addrs` entry so the
    /// owning VPC layer can represent SLAAC; this model does not itself know
    /// or enforce the VPC's allocation mode. IPv4 interfaces may not retain a
    /// prefix without an address.
    ///
    /// TODO(chet): Allow a default value to be set here for backwards
    /// compatibility, since InstanceInterfaceConfigs for existing instances
    /// won't have this information stored.
    #[serde(
        default,
        deserialize_with = "deserialize_network_prefix_id_ipnetwork_map",
        serialize_with = "serialize_network_prefix_id_ipnetwork_map"
    )]
    pub interface_prefixes: HashMap<NetworkPrefixId, IpNetwork>,

    /// The gateway (with prefix) for each network segment
    #[serde(
        default,
        deserialize_with = "deserialize_network_prefix_id_ipnetwork_map",
        serialize_with = "serialize_network_prefix_id_ipnetwork_map"
    )]
    pub network_segment_gateways: HashMap<NetworkPrefixId, IpNetwork>,

    /// The MAC address of the NIC, if this is zero-DPU instance with host inband networking. For
    /// zero-DPU instances, the instance interface is just the host's network interface, so we can
    /// assign the host's MAC here. This is opposed to instances with DPUs, where we do not know the
    /// MAC address that the instance will see until we start getting status observations from the
    /// forge agent.
    pub host_inband_mac_address: Option<MacAddress>,

    /// The DPU device this interface corresponds to.  The device/instance pair will be mapped to a specific DPU
    pub device_locator: Option<DeviceLocator>,

    /// An internal ID used to associate an interface status with the interface config
    pub internal_uuid: uuid::Uuid,

    /// Logical VPC ownership for this resolved interface.
    ///
    /// For legacy segment-bound allocations this is derived from the segment.
    /// For Flat zero-DPU auto allocations this is copied from
    /// [`InstanceNetworkConfig::vpc_id`] after HostInband segment resolution.
    pub vpc_id: Option<VpcId>,
}

impl InstanceInterfaceConfig {
    /// Returns the resolved VPC prefix IDs keyed by address family.
    pub fn resolved_vpc_prefixes(&self) -> Option<InstanceInterfaceResolvedVpcPrefixes> {
        let primary_vpc_prefix_id = match self.network_details.as_ref() {
            Some(NetworkDetails::VpcPrefixId(vpc_prefix_id)) => *vpc_prefix_id,
            _ => return None,
        };

        let ipv6_vpc_prefix_id = self
            .ipv6_interface_config
            .as_ref()
            .map(|ipv6| ipv6.vpc_prefix_id);

        if let Some(selection) = self.vpc_selection {
            return Some(match selection.family_mode {
                InstanceInterfaceIpFamilyMode::Ipv4Only => InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: Some(primary_vpc_prefix_id),
                    ipv6_vpc_prefix_id: None,
                },
                InstanceInterfaceIpFamilyMode::Ipv6Only => InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: None,
                    ipv6_vpc_prefix_id: Some(primary_vpc_prefix_id),
                },
                InstanceInterfaceIpFamilyMode::DualStack => InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: Some(primary_vpc_prefix_id),
                    ipv6_vpc_prefix_id,
                },
            });
        }

        // In explicit-prefix storage, an IPv6 sidecar represents dual stack,
        // with `network_details` holding the primary IPv4 prefix.
        if ipv6_vpc_prefix_id.is_some() {
            return Some(InstanceInterfaceResolvedVpcPrefixes {
                ipv4_vpc_prefix_id: Some(primary_vpc_prefix_id),
                ipv6_vpc_prefix_id,
            });
        }

        let primary_is_ipv6 = if let Some(requested_address) = self.requested_ip_addr {
            // Explicit address intent is validated against the primary prefix
            // and is therefore authoritative for its family.
            requested_address.is_ipv6()
        } else {
            // Canonical dual-stack records returned above. Infer IPv6 only from
            // consistently IPv6 persisted state so a sidecar-less mixed legacy
            // record retains its primary prefix's IPv4 classification.
            let mut family_evidence = self
                .ip_addrs
                .values()
                .map(|address| address.is_ipv6())
                .chain(
                    self.interface_prefixes
                        .values()
                        .map(|prefix| prefix.is_ipv6()),
                )
                .chain(
                    self.network_segment_gateways
                        .values()
                        .map(|gateway| gateway.is_ipv6()),
                );

            matches!(family_evidence.next(), Some(true))
                && family_evidence.all(std::convert::identity)
        };

        // Canonical dual-stack records returned above, so this fallback
        // projects only the remaining primary prefix's family.
        Some(if primary_is_ipv6 {
            InstanceInterfaceResolvedVpcPrefixes {
                ipv4_vpc_prefix_id: None,
                ipv6_vpc_prefix_id: Some(primary_vpc_prefix_id),
            }
        } else {
            // Existing family-agnostic records predate IPv6 allocation and
            // therefore use IPv4 when no persisted family evidence exists.
            InstanceInterfaceResolvedVpcPrefixes {
                ipv4_vpc_prefix_id: Some(primary_vpc_prefix_id),
                ipv6_vpc_prefix_id: None,
            }
        })
    }

    /// Returns the generated segment associated with resolved explicit or
    /// automatic VPC-prefix intent.
    pub fn generated_network_segment_id(&self) -> Option<NetworkSegmentId> {
        self.resolved_vpc_prefixes()?;
        self.network_segment_id
    }

    /// Returns true if this instance interface is equivalent to the host's in-band interface,
    /// meaning it belong to a network segment of type [`NetworkSegmentType::HostInband`]. This is
    /// in contrast to DPU-based interfaces where the instance sees an overlay network.
    ///
    /// Currently this is true if self.host_inband_mac_address is set to some value.
    pub fn is_host_inband(&self) -> bool {
        self.host_inband_mac_address.is_some()
    }
}

/// Minimum valid value (inclusive) for a virtual function ID
pub const INTERFACE_VFID_MIN: u8 = 0;
/// Maximum valid value (inclusive) for a virtual function ID
pub const INTERFACE_VFID_MAX: u8 = 15;

pub fn deserialize_network_prefix_id_ipaddr_map<'de, D>(
    deserializer: D,
) -> Result<HashMap<NetworkPrefixId, IpAddr>, D::Error>
where
    D: Deserializer<'de>,
{
    let uuid_map = <HashMap<uuid::Uuid, IpAddr>>::deserialize(deserializer)?;
    Ok(uuid_map
        .into_iter()
        .map(|(uuid, ipaddr)| (NetworkPrefixId::from(uuid), ipaddr))
        .collect())
}

pub fn deserialize_network_prefix_id_ipnetwork_map<'de, D>(
    deserializer: D,
) -> Result<HashMap<NetworkPrefixId, IpNetwork>, D::Error>
where
    D: Deserializer<'de>,
{
    let uuid_map = <HashMap<uuid::Uuid, IpNetwork>>::deserialize(deserializer)?;
    Ok(uuid_map
        .into_iter()
        .map(|(uuid, ipnetwork)| (NetworkPrefixId::from(uuid), ipnetwork))
        .collect())
}

pub fn serialize_network_prefix_id_ipaddr_map<S>(
    map: &HashMap<NetworkPrefixId, IpAddr>,
    s: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut out_map = s.serialize_map(Some(map.len()))?;
    for (k, v) in map {
        let uuid: uuid::Uuid = (*k).into();
        out_map.serialize_entry(&uuid, v)?
    }
    out_map.end()
}

pub fn serialize_network_prefix_id_ipnetwork_map<S>(
    map: &HashMap<NetworkPrefixId, IpNetwork>,
    s: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut out_map = s.serialize_map(Some(map.len()))?;
    for (k, v) in map {
        let uuid: uuid::Uuid = (*k).into();
        out_map.serialize_entry(&uuid, v)?
    }
    out_map.end()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    #[test]
    fn iterate_function_ids() {
        let func_ids: Vec<InterfaceFunctionId> = InterfaceFunctionId::iter_all().collect();
        assert_eq!(
            func_ids.len(),
            2 + INTERFACE_VFID_MAX as usize - INTERFACE_VFID_MIN as usize
        );

        assert_eq!(func_ids[0], InterfaceFunctionId::Physical {});
        for (i, func_id) in func_ids[1..].iter().enumerate() {
            assert_eq!(
                *func_id,
                InterfaceFunctionId::Virtual {
                    id: (INTERFACE_VFID_MIN + i as u8)
                }
            );
        }
    }

    // Serde JSON round-trip for each InterfaceFunctionId variant: each row
    // asserts the exact serialized form, and the closure confirms the value
    // round-trips back equal before yielding that string.
    #[test]
    fn serialize_function_id() {
        scenarios!(
            // Serialize, confirm it round-trips back equal, then yield the JSON.
            // serde_json::Error is not PartialEq, so collapse failures to ().
            run = |function_id| {
                let serialized = serde_json::to_string(&function_id).map_err(|_| ())?;
                let round_tripped =
                    serde_json::from_str::<InterfaceFunctionId>(&serialized).map_err(|_| ())?;
                assert_eq!(round_tripped, function_id);
                Ok::<_, ()>(serialized)
            };
            "physical" {
                InterfaceFunctionId::Physical {} => Yields(r#"{"type":"physical"}"#.to_string()),
            }

            "virtual" {
                InterfaceFunctionId::Virtual { id: 24 } => Yields(r#"{"type":"virtual","id":24}"#.to_string()),
            }
        );
    }

    #[test]
    fn serialize_interface_config() {
        let function_id = InterfaceFunctionId::Physical {};
        let network_segment_id: NetworkSegmentId =
            uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1200").into();
        let network_prefix_1 =
            NetworkPrefixId::from(uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1201"));
        let ip_addrs = HashMap::from([(network_prefix_1, "192.168.1.2".parse().unwrap())]);
        let requested_ip_addr = Some("192.168.1.2".parse().unwrap());
        let interface_prefixes =
            HashMap::from([(network_prefix_1, "192.168.1.2/32".parse().unwrap())]);
        let network_segment_gateways = HashMap::default();
        let internal_uuid = uuid::uuid!("37c3dc65-9aef-4439-b7ca-d532a0a41d7f");

        let interface = InstanceInterfaceConfig {
            function_id,
            network_segment_id: Some(network_segment_id),
            ip_addrs,
            requested_ip_addr,
            ipv6_interface_config: None,
            routing_profile: None,
            interface_prefixes,
            network_segment_gateways,
            host_inband_mac_address: None,
            network_details: None,
            vpc_selection: None,
            device_locator: None,
            internal_uuid,
            vpc_id: None,
        };
        let serialized = serde_json::to_string(&interface).unwrap();
        assert_eq!(
            serialized,
            r#"{"function_id":{"type":"physical"},"network_details":null,"network_segment_id":"91609f10-c91d-470d-a260-6293ea0c1200","ip_addrs":{"91609f10-c91d-470d-a260-6293ea0c1201":"192.168.1.2"},"requested_ip_addr":"192.168.1.2","ipv6":null,"routing_profile":null,"interface_prefixes":{"91609f10-c91d-470d-a260-6293ea0c1201":"192.168.1.2/32"},"network_segment_gateways":{},"host_inband_mac_address":null,"device_locator":null,"internal_uuid":"37c3dc65-9aef-4439-b7ca-d532a0a41d7f","vpc_id":null}"#
        );

        assert_eq!(
            serde_json::from_str::<InstanceInterfaceConfig>(&serialized).unwrap(),
            interface
        );
    }

    /// Creates a valid instance network configuration using the maximum
    /// amount of interface
    const BASE_SEGMENT_ID: uuid::Uuid = uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c0000");
    fn offset_segment_id(offset: u8) -> NetworkSegmentId {
        uuid::Uuid::from_u128(BASE_SEGMENT_ID.as_u128() + offset as u128).into()
    }

    fn create_valid_network_config() -> InstanceNetworkConfig {
        let interfaces: Vec<InstanceInterfaceConfig> = InterfaceFunctionId::iter_all()
            .enumerate()
            .map(|(idx, function_id)| {
                let network_segment_id = offset_segment_id(idx as u8);
                InstanceInterfaceConfig {
                    function_id,
                    network_segment_id: Some(network_segment_id),
                    ip_addrs: HashMap::default(),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::default(),
                    network_segment_gateways: HashMap::default(),
                    host_inband_mac_address: None,
                    network_details: None,
                    vpc_selection: None,
                    device_locator: None,
                    internal_uuid: uuid::Uuid::new_v4(),
                    vpc_id: None,
                }
            })
            .collect();

        InstanceNetworkConfig {
            interfaces,
            auto_config: None,
        }
    }

    #[test]
    fn operator_managed_networking_requires_nonempty_host_inband_interfaces() {
        let empty = InstanceNetworkConfig::default();
        let dpu_networked = create_valid_network_config();

        let mut host_inband = create_valid_network_config();
        for interface in &mut host_inband.interfaces {
            interface.host_inband_mac_address = Some(MacAddress::new([1, 2, 3, 4, 5, 6]));
        }

        let mut mixed = host_inband.clone();
        mixed.interfaces[0].host_inband_mac_address = None;

        value_scenarios!(
            run = |config: InstanceNetworkConfig| config.uses_operator_managed_networking();
            "operator-managed" {
                host_inband => true,
            }
            "not operator-managed" {
                empty => false,
                dpu_networked => false,
                mixed => false,
            }
        );
    }

    /// Builds one resolved automatic interface while retaining the usual
    /// service-generated representation for its selected prefixes.
    fn resolved_vpc_interface(
        selection: InstanceInterfaceVpcSelection,
        primary_vpc_prefix_id: VpcPrefixId,
        ipv6_vpc_prefix_id: Option<VpcPrefixId>,
    ) -> InstanceInterfaceConfig {
        let mut interface = create_valid_network_config().interfaces.swap_remove(0);
        interface.network_details = Some(NetworkDetails::VpcPrefixId(primary_vpc_prefix_id));
        interface.vpc_selection = Some(selection);
        interface.ipv6_interface_config =
            ipv6_vpc_prefix_id.map(|vpc_prefix_id| Ipv6InterfaceConfig {
                vpc_prefix_id,
                requested_ip_addr: None,
            });
        interface.vpc_id = Some(selection.vpc_id);
        interface
    }

    /// Resolved prefixes are projected by family rather than by the storage
    /// position used for rolling compatibility.
    #[test]
    fn resolved_vpc_prefixes_follow_family_mode() {
        let vpc_id = VpcId::new();
        let ipv4_vpc_prefix_id = VpcPrefixId::new();
        let ipv6_vpc_prefix_id = VpcPrefixId::new();

        value_scenarios!(
            run = |(family_mode, primary_vpc_prefix_id, secondary_vpc_prefix_id)| {
                resolved_vpc_interface(
                    InstanceInterfaceVpcSelection {
                        vpc_id,
                        family_mode,
                    },
                    primary_vpc_prefix_id,
                    secondary_vpc_prefix_id,
                )
                .resolved_vpc_prefixes()
            };
            "IPv4 only" {
                (
                    InstanceInterfaceIpFamilyMode::Ipv4Only,
                    ipv4_vpc_prefix_id,
                    None,
                ) => Some(InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: Some(ipv4_vpc_prefix_id),
                    ipv6_vpc_prefix_id: None,
                }),
            }
            "IPv6 only" {
                (
                    InstanceInterfaceIpFamilyMode::Ipv6Only,
                    ipv6_vpc_prefix_id,
                    None,
                ) => Some(InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: None,
                    ipv6_vpc_prefix_id: Some(ipv6_vpc_prefix_id),
                }),
            }
            "dual stack" {
                (
                    InstanceInterfaceIpFamilyMode::DualStack,
                    ipv4_vpc_prefix_id,
                    Some(ipv6_vpc_prefix_id),
                ) => Some(InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: Some(ipv4_vpc_prefix_id),
                    ipv6_vpc_prefix_id: Some(ipv6_vpc_prefix_id),
                }),
            }
        );
    }

    /// Legacy explicit-prefix storage uses requested or persisted family
    /// evidence and treats an IPv6 sidecar as explicit dual stack. Absent or
    /// mixed evidence defaults to IPv4 so a sidecar lost during an older
    /// rolling deployment cannot relabel the primary IPv4 prefix as IPv6.
    #[test]
    fn resolved_vpc_prefixes_support_explicit_prefix_storage() {
        // Share one VPC and distinct family IDs so each scenario isolates
        // which family slot receives the stored primary prefix.
        let vpc_id = VpcId::new();
        let ipv4_vpc_prefix_id = VpcPrefixId::new();
        let ipv6_vpc_prefix_id = VpcPrefixId::new();

        // Clear selection metadata without adding family evidence to model a
        // legacy explicit record, whose primary prefix defaults to IPv4.
        let mut ipv4 = resolved_vpc_interface(
            InstanceInterfaceVpcSelection {
                vpc_id,
                family_mode: InstanceInterfaceIpFamilyMode::Ipv4Only,
            },
            ipv4_vpc_prefix_id,
            None,
        );
        ipv4.vpc_selection = None;

        // A requested IPv6 address belongs to the primary explicit prefix and
        // therefore provides authoritative IPv6 family evidence.
        let mut ipv6 = resolved_vpc_interface(
            InstanceInterfaceVpcSelection {
                vpc_id,
                family_mode: InstanceInterfaceIpFamilyMode::Ipv6Only,
            },
            ipv6_vpc_prefix_id,
            None,
        );
        ipv6.vpc_selection = None;
        ipv6.requested_ip_addr = Some("2001:db8::10".parse().unwrap());

        // Remove requested intent to verify that persisted allocation state
        // can independently identify an IPv6-only primary.
        let mut allocated_ipv6 = ipv6.clone();
        allocated_ipv6.requested_ip_addr = None;
        allocated_ipv6
            .ip_addrs
            .insert(NetworkPrefixId::new(), "2001:db8::10".parse().unwrap());

        // An IPv6 sidecar models canonical explicit dual stack, where IPv4 is
        // primary and both family-specific parent IDs must remain visible.
        let mut dual_stack = resolved_vpc_interface(
            InstanceInterfaceVpcSelection {
                vpc_id,
                family_mode: InstanceInterfaceIpFamilyMode::DualStack,
            },
            ipv4_vpc_prefix_id,
            Some(ipv6_vpc_prefix_id),
        );
        dual_stack.vpc_selection = None;

        // Removing the sidecar models an older rolling rewrite that retained
        // both address families; the surviving primary ID must remain IPv4.
        let mut mixed_without_sidecar = dual_stack.clone();
        mixed_without_sidecar.ipv6_interface_config = None;
        mixed_without_sidecar.ip_addrs = HashMap::from([
            (NetworkPrefixId::new(), "192.0.2.10".parse().unwrap()),
            (NetworkPrefixId::new(), "2001:db8::10".parse().unwrap()),
        ]);

        value_scenarios!(
            run = |interface| interface.resolved_vpc_prefixes();
            "IPv4 only" {
                ipv4 => Some(InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: Some(ipv4_vpc_prefix_id),
                    ipv6_vpc_prefix_id: None,
                }),
            }
            "IPv6 only from requested address" {
                ipv6 => Some(InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: None,
                    ipv6_vpc_prefix_id: Some(ipv6_vpc_prefix_id),
                }),
            }
            "IPv6 only from allocated address" {
                allocated_ipv6 => Some(InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: None,
                    ipv6_vpc_prefix_id: Some(ipv6_vpc_prefix_id),
                }),
            }
            "dual stack" {
                dual_stack => Some(InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: Some(ipv4_vpc_prefix_id),
                    ipv6_vpc_prefix_id: Some(ipv6_vpc_prefix_id),
                }),
            }
            "mixed legacy evidence without sidecar" {
                mixed_without_sidecar => Some(InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: Some(ipv4_vpc_prefix_id),
                    ipv6_vpc_prefix_id: None,
                }),
            }
        );
    }

    /// Automatic selection metadata round-trips additively while the explicit
    /// resolution remains readable if an older representation ignores it.
    #[test]
    fn serialize_resolved_vpc_selection_additively() {
        let selection = InstanceInterfaceVpcSelection {
            vpc_id: VpcId::new(),
            family_mode: InstanceInterfaceIpFamilyMode::DualStack,
        };
        let interface =
            resolved_vpc_interface(selection, VpcPrefixId::new(), Some(VpcPrefixId::new()));

        let mut serialized = serde_json::to_value(&interface).unwrap();
        assert!(serialized.get("vpc_selection").is_some());
        assert_eq!(
            serde_json::from_value::<InstanceInterfaceConfig>(serialized.clone()).unwrap(),
            interface
        );

        // Dropping the additive field models a rolling peer that understands
        // only the retained explicit-prefix storage representation.
        serialized.as_object_mut().unwrap().remove("vpc_selection");
        let legacy_view = serde_json::from_value::<InstanceInterfaceConfig>(serialized).unwrap();
        assert_eq!(legacy_view.vpc_selection, None);
        assert_eq!(legacy_view.network_details, interface.network_details);
        assert_eq!(
            legacy_view.ipv6_interface_config,
            interface.ipv6_interface_config
        );
    }

    /// Update comparison ignores generated automatic resolution, but still
    /// detects every caller-controlled selection change.
    #[test]
    fn network_update_detection_compares_vpc_selection_intent() {
        let selection = InstanceInterfaceVpcSelection {
            vpc_id: VpcId::new(),
            family_mode: InstanceInterfaceIpFamilyMode::Ipv4Only,
        };
        let mut current = create_valid_network_config();
        current.interfaces.truncate(1);
        current.interfaces[0] = resolved_vpc_interface(selection, VpcPrefixId::new(), None);

        // A repeated caller request carries only the unchanged VPC and family
        // intent; absent generated resolution must not make it an update.
        let mut unresolved = current.clone();
        unresolved.interfaces[0].network_details = None;
        unresolved.interfaces[0].network_segment_id = None;

        // Prefix and segment IDs are service-generated resolution, so changing
        // only those fields must not make identical caller intent an update.
        let mut alternate_resolution = current.clone();
        alternate_resolution.interfaces[0].network_details =
            Some(NetworkDetails::VpcPrefixId(VpcPrefixId::new()));
        alternate_resolution.interfaces[0].network_segment_id = Some(offset_segment_id(42));

        let mut changed_family = unresolved.clone();
        changed_family.interfaces[0].vpc_selection = Some(InstanceInterfaceVpcSelection {
            family_mode: InstanceInterfaceIpFamilyMode::DualStack,
            ..selection
        });

        let mut changed_vpc = unresolved.clone();
        changed_vpc.interfaces[0].vpc_selection = Some(InstanceInterfaceVpcSelection {
            vpc_id: VpcId::new(),
            ..selection
        });

        value_scenarios!(
            run = |requested| current.is_network_config_update_requested(&requested);
            "unresolved repetition" {
                unresolved => false,
            }
            "same intent with alternate generated resolution" {
                alternate_resolution => false,
            }
            "changed family mode" {
                changed_family => true,
            }
            "changed VPC" {
                changed_vpc => true,
            }
        );
    }

    /// An unresolved repetition inherits the active automatic resolution and
    /// is returned as a common resource for cleanup filtering.
    #[test]
    fn copy_existing_resources_resolves_matching_vpc_selection() {
        let selection = InstanceInterfaceVpcSelection {
            vpc_id: VpcId::new(),
            family_mode: InstanceInterfaceIpFamilyMode::Ipv4Only,
        };
        let mut current = create_valid_network_config();
        current.interfaces.truncate(1);
        current.interfaces[0] = resolved_vpc_interface(selection, VpcPrefixId::new(), None);
        let network_prefix_id = NetworkPrefixId::new();
        current.interfaces[0]
            .ip_addrs
            .insert(network_prefix_id, "192.0.2.10".parse().unwrap());
        current.interfaces[0]
            .interface_prefixes
            .insert(network_prefix_id, "192.0.2.10/32".parse().unwrap());
        current.interfaces[0]
            .network_segment_gateways
            .insert(network_prefix_id, "192.0.2.1/24".parse().unwrap());

        let expected_resolution = current.interfaces[0].resolved_vpc_prefixes();
        let expected_segment_id = current.interfaces[0].network_segment_id;
        let expected_ip_addrs = current.interfaces[0].ip_addrs.clone();
        let mut requested = current.clone();
        requested.interfaces[0].network_details = None;
        requested.interfaces[0].network_segment_id = None;
        requested.interfaces[0].vpc_id = None;
        requested.interfaces[0].ip_addrs.clear();
        requested.interfaces[0].interface_prefixes.clear();
        requested.interfaces[0].network_segment_gateways.clear();

        let common = requested.copy_existing_resources(&current);

        assert_eq!(common.len(), 1);
        assert_eq!(
            requested.interfaces[0].resolved_vpc_prefixes(),
            expected_resolution
        );
        assert_eq!(
            requested.interfaces[0].network_segment_id,
            expected_segment_id
        );
        assert_eq!(requested.interfaces[0].ip_addrs, expected_ip_addrs);
    }

    /// Switching active explicit prefixes to matching automatic intent reuses
    /// their generated resources without retaining old explicit IP requests.
    #[test]
    fn copy_existing_resources_reuses_explicit_prefix_for_vpc_selection() {
        // Build the current explicit dual-stack config with generated allocation state.
        let vpc_id = VpcId::new();
        let ipv4_vpc_prefix_id = VpcPrefixId::new();
        let ipv6_vpc_prefix_id = VpcPrefixId::new();
        let selection = InstanceInterfaceVpcSelection {
            vpc_id,
            family_mode: InstanceInterfaceIpFamilyMode::DualStack,
        };
        let mut current = create_valid_network_config();
        current.interfaces.truncate(1);
        current.interfaces[0].network_details =
            Some(NetworkDetails::VpcPrefixId(ipv4_vpc_prefix_id));
        current.interfaces[0].vpc_id = Some(vpc_id);
        current.interfaces[0].requested_ip_addr = Some("192.0.2.10".parse().unwrap());
        current.interfaces[0].ipv6_interface_config = Some(Ipv6InterfaceConfig {
            vpc_prefix_id: ipv6_vpc_prefix_id,
            requested_ip_addr: Some("2001:db8::10".parse().unwrap()),
        });
        let ipv4_network_prefix_id = NetworkPrefixId::new();
        let ipv6_network_prefix_id = NetworkPrefixId::new();
        current.interfaces[0]
            .ip_addrs
            .insert(ipv4_network_prefix_id, "192.0.2.10".parse().unwrap());
        current.interfaces[0]
            .ip_addrs
            .insert(ipv6_network_prefix_id, "2001:db8::10".parse().unwrap());
        current.interfaces[0]
            .interface_prefixes
            .insert(ipv4_network_prefix_id, "192.0.2.10/32".parse().unwrap());
        current.interfaces[0]
            .interface_prefixes
            .insert(ipv6_network_prefix_id, "2001:db8::10/128".parse().unwrap());
        current.interfaces[0]
            .network_segment_gateways
            .insert(ipv4_network_prefix_id, "192.0.2.1/24".parse().unwrap());
        current.interfaces[0]
            .network_segment_gateways
            .insert(ipv6_network_prefix_id, "2001:db8::1/64".parse().unwrap());
        let expected_segment_id = current.interfaces[0].network_segment_id;
        let expected_ip_addrs = current.interfaces[0].ip_addrs.clone();

        // Request matching automatic intent with generated fields unset.
        let mut requested = create_valid_network_config();
        requested.interfaces.truncate(1);
        requested.interfaces[0].vpc_selection = Some(selection);
        requested.interfaces[0].network_segment_id = None;

        // Reconcile the update and verify resources are reused without explicit IP intent.
        assert!(current.is_network_config_update_requested(&requested));
        let common = requested.copy_existing_resources(&current);

        assert_eq!(common.len(), 1);
        assert_eq!(
            requested.interfaces[0].resolved_vpc_prefixes(),
            Some(InstanceInterfaceResolvedVpcPrefixes {
                ipv4_vpc_prefix_id: Some(ipv4_vpc_prefix_id),
                ipv6_vpc_prefix_id: Some(ipv6_vpc_prefix_id),
            })
        );
        assert_eq!(
            requested.interfaces[0].network_segment_id,
            expected_segment_id
        );
        assert_eq!(requested.interfaces[0].ip_addrs, expected_ip_addrs);
        assert_eq!(requested.interfaces[0].requested_ip_addr, None);
        assert_eq!(
            requested.interfaces[0]
                .ipv6_interface_config
                .as_ref()
                .and_then(|ipv6| ipv6.requested_ip_addr),
            None
        );
        assert_eq!(requested.interfaces[0].vpc_selection, Some(selection));
    }

    /// Requests already resolved to a different prefix or segment must not
    /// reuse or protect the active resources during cleanup.
    #[test]
    fn copy_existing_resources_preserves_alternate_resolution() {
        let selection = InstanceInterfaceVpcSelection {
            vpc_id: VpcId::new(),
            family_mode: InstanceInterfaceIpFamilyMode::Ipv4Only,
        };
        let mut current = create_valid_network_config();
        current.interfaces.truncate(1);
        current.interfaces[0] = resolved_vpc_interface(selection, VpcPrefixId::new(), None);

        let mut alternate_prefix = create_valid_network_config();
        alternate_prefix.interfaces.truncate(1);
        alternate_prefix.interfaces[0] =
            resolved_vpc_interface(selection, VpcPrefixId::new(), None);

        let mut alternate_segment = current.clone();
        alternate_segment.interfaces[0].network_segment_id = Some(offset_segment_id(42));

        value_scenarios!(
            run = |mut requested| {
                let expected_resolution = requested.interfaces[0].resolved_vpc_prefixes();
                let expected_segment_id = requested.interfaces[0].network_segment_id;
                let common = requested.copy_existing_resources(&current);
                common.is_empty()
                    && requested.interfaces[0].resolved_vpc_prefixes() == expected_resolution
                    && requested.interfaces[0].network_segment_id == expected_segment_id
            };
            "different selected prefix" {
                alternate_prefix => true,
            }
            "different generated segment" {
                alternate_segment => true,
            }
        );
    }

    #[test]
    fn network_update_detection_ignores_derived_vpc_id() {
        let mut current = create_valid_network_config();
        let mut requested = current.clone();

        current.interfaces[0].vpc_id = Some(VpcId::new());
        requested.interfaces[0].vpc_id = None;
        requested.interfaces[0].internal_uuid = uuid::Uuid::new_v4();

        assert!(!current.is_network_config_update_requested(&requested));
    }

    #[test]
    fn copy_existing_resources_preserves_derived_vpc_id() {
        let vpc_id = VpcId::new();
        let mut current = create_valid_network_config();
        current.interfaces[0].vpc_id = Some(vpc_id);

        let mut requested = create_valid_network_config();
        requested.copy_existing_resources(&current);

        assert_eq!(requested.interfaces[0].vpc_id, Some(vpc_id));
    }

    /// HostInband MACs come from Core rather than the caller, so a matching
    /// interface must retain that physical-interface identity with its IPs.
    #[test]
    fn copy_existing_resources_preserves_host_inband_mac() {
        let host_inband_mac_address = MacAddress::new([1, 2, 3, 4, 5, 6]);
        let mut current = create_valid_network_config();
        current.interfaces[0].host_inband_mac_address = Some(host_inband_mac_address);

        let mut requested = create_valid_network_config();
        requested.copy_existing_resources(&current);

        assert_eq!(
            requested.interfaces[0].host_inband_mac_address,
            Some(host_inband_mac_address)
        );
    }

    // InstanceNetworkConfig::validate over a base valid config mutated per row.
    // Input is (config, allow_instance_vf). ConfigValidationError is not
    // PartialEq, so rejections assert only that validation errs (Fails); the
    // exact error value is not part of the contract here.
    #[test]
    fn validate_network_config() {
        const DUPLICATE_SEGMENT_ID: uuid::Uuid =
            uuid::uuid!("91609f10-c91d-470d-a260-1234560c0000");

        let valid = create_valid_network_config();

        let virtual_functions_disabled = create_valid_network_config();

        let mut duplicate_virtual_function = create_valid_network_config();
        duplicate_virtual_function.interfaces[2].function_id =
            InterfaceFunctionId::Virtual { id: 0 };

        let mut out_of_bounds_virtual_function = create_valid_network_config();
        out_of_bounds_virtual_function.interfaces[2].function_id =
            InterfaceFunctionId::Virtual { id: 16 };

        let mut no_physical_function = create_valid_network_config();
        no_physical_function.interfaces.swap_remove(0);

        let mut missing_middle_virtual_function = create_valid_network_config();
        missing_middle_virtual_function
            .interfaces
            .swap_remove(INTERFACE_VFID_MAX as usize + 1);

        let mut duplicate_network_segment = create_valid_network_config();
        duplicate_network_segment.interfaces[0].network_segment_id =
            Some(DUPLICATE_SEGMENT_ID.into());
        duplicate_network_segment.interfaces[1].network_segment_id =
            Some(DUPLICATE_SEGMENT_ID.into());

        let mut slaac_ipv6_prefix_without_address = create_valid_network_config();
        slaac_ipv6_prefix_without_address.interfaces[0]
            .interface_prefixes
            .insert(NetworkPrefixId::new(), "2001:db8::/127".parse().unwrap());

        let mut ipv4_prefix_without_address = create_valid_network_config();
        ipv4_prefix_without_address.interfaces[0]
            .interface_prefixes
            .insert(NetworkPrefixId::new(), "192.0.2.10/32".parse().unwrap());

        let mut address_without_prefix = create_valid_network_config();
        address_without_prefix.interfaces[0]
            .ip_addrs
            .insert(NetworkPrefixId::new(), "192.0.2.10".parse().unwrap());
        address_without_prefix.interfaces[0]
            .interface_prefixes
            .insert(NetworkPrefixId::new(), "2001:db8::/127".parse().unwrap());

        scenarios!(
            run = |(config, allow_instance_vf)| config.validate(allow_instance_vf).map_err(drop);
            "valid config with virtual functions allowed" {
                (valid, true) => Yields(()),
            }

            "virtual functions disabled by site configuration" {
                (virtual_functions_disabled, false) => Fails,
            }

            "duplicate virtual function id" {
                (duplicate_virtual_function, true) => Fails,
            }

            "out of bounds virtual function id" {
                (out_of_bounds_virtual_function, true) => Fails,
            }

            "no physical function" {
                (no_physical_function, true) => Fails,
            }

            "missing middle virtual function id is allowed" {
                (missing_middle_virtual_function, true) => Yields(()),
            }

            "duplicate network segment" {
                (duplicate_network_segment, true) => Fails,
            }

            "SLAAC IPv6 prefix without a concrete address" {
                (slaac_ipv6_prefix_without_address, true) => Yields(()),
            }

            "IPv4 prefix without a concrete address" {
                (ipv4_prefix_without_address, true) => Fails,
            }

            "address without its interface prefix" {
                (address_without_prefix, true) => Fails,
            }
        );
    }

    /// A resolved `auto` config as `add_inband_interfaces_to_config` produces
    /// it: one bare `Physical {}` interface per HostInband segment, no
    /// `device_locator`. Interface identity is the segment.
    fn create_resolved_auto_network_config(segment_count: u8) -> InstanceNetworkConfig {
        InstanceNetworkConfig {
            interfaces: (0..segment_count)
                .map(|idx| InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Physical {},
                    network_segment_id: Some(offset_segment_id(idx)),
                    network_details: None,
                    vpc_selection: None,
                    ip_addrs: HashMap::default(),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::default(),
                    network_segment_gateways: HashMap::default(),
                    host_inband_mac_address: None,
                    device_locator: None,
                    internal_uuid: uuid::Uuid::new_v4(),
                    vpc_id: None,
                })
                .collect(),
            auto_config: Some(InstanceNetworkAutoConfig {
                vpc_id: VpcId::new(),
            }),
        }
    }

    // InstanceNetworkConfig::validate over resolved `auto` configs (the
    // output of `add_inband_interfaces_to_config`). A multi-NIC zero-DPU
    // host resolves to several bare `Physical {}` interfaces -- one per
    // HostInband segment -- and must validate; the per-device function-id
    // bucketing would read those as duplicates of one device.
    #[test]
    fn validate_resolved_auto_network_config() {
        let single_segment = create_resolved_auto_network_config(1);
        let multi_segment = create_resolved_auto_network_config(3);

        let mut duplicate_segment = create_resolved_auto_network_config(2);
        duplicate_segment.interfaces[1].network_segment_id =
            duplicate_segment.interfaces[0].network_segment_id;

        let mut virtual_function = create_resolved_auto_network_config(2);
        virtual_function.interfaces[1].function_id = InterfaceFunctionId::Virtual { id: 0 };

        let mut located_interface = create_resolved_auto_network_config(2);
        located_interface.interfaces[1].device_locator = Some(DeviceLocator {
            device: "DPU".to_string(),
            device_instance: 0,
        });

        scenarios!(
            run = |(config, allow_instance_vf)| config.validate(allow_instance_vf).map_err(drop);
            "single resolved host-inband interface" {
                (single_segment, false) => Yields(()),
            }

            "one interface per HostInband segment on a multi-NIC host" {
                (multi_segment, false) => Yields(()),
            }

            "duplicate segments are still rejected for auto configs" {
                (duplicate_segment, false) => Fails,
            }

            "virtual functions cannot appear in an auto config" {
                (virtual_function, true) => Fails,
            }

            "device-located interfaces cannot appear in an auto config" {
                (located_interface, false) => Fails,
            }
        );
    }
}
