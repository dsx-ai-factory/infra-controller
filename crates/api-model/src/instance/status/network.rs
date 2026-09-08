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

use std::collections::HashMap;
use std::convert::Into;
use std::net::IpAddr;

use carbide_uuid::machine::DpuMachineId;
use carbide_uuid::vpc::VpcId;
use chrono::{DateTime, Utc};
use config_version::{ConfigVersion, Versioned};
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

use crate::SerializableMacAddress;
use crate::instance::config::network::{
    InstanceInterfaceConfig, InstanceInterfaceResolvedVpcPrefixes, InstanceNetworkConfig,
    InterfaceFunctionId,
};
use crate::instance::status::SyncState;
use crate::machine::Machine;
use crate::network_security_group::NetworkSecurityGroupStatusObservation;

/// Status of the networking subsystem of an instance
///
/// The status report is only valid against one particular version of
/// [InstanceInterfaceConfig](crate::model::instance::config::network::InstanceInterfaceConfig). It can not be interpreted without it, since
/// e.g. the amount and configuration of network interfaces can change between
/// configs.
///
/// Since the user can change the configuration at any point in time for an instance,
/// we can not directly store this status in the database - it might not match
/// the newest config anymore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceNetworkStatus {
    /// Status for each configured interface
    ///
    /// For non-auto configs: each entry in this status array maps to its
    /// corresponding entry in the Config section. E.g.
    /// `instance.status.network.interfaces[1]` maps to
    /// `instance.config.network.interfaces[1]`.
    ///
    /// For auto configs (`InstanceNetworkConfig.auto = true`): on the wire
    /// the config-side `interfaces` is always empty (the request is
    /// preserved verbatim), so this array stands alone and describes the
    /// interfaces Carbide resolved from the host's HostInband segments.
    /// There is no positional relationship to consult on the config side.
    pub interfaces: Vec<InstanceInterfaceStatus>,

    /// Whether all desired network changes that the user has applied have taken effect
    /// This includes:
    /// - Whether `InstanceNetworkConfig` is of exactly the same version as the
    ///   version the user desires.
    /// - Whether the version of each security policy that is either directly referenced
    ///   as part of an `InstanceInterfaceConfig` or indirectly referenced via the
    ///   the security policies that are applied to the VPC or NetworkSegment
    ///   is exactly the same version as the version the user desires.
    ///
    /// Note for the implementation: We need to monitor all these config versions
    /// on the feedback path from DPU to carbide in order to know whether the
    /// changes have indeed taken effect.
    /// TODO: Do we also want to show all applied versions here, or just track them
    /// internally? Probably not helpful for tenants at all - but it could be helpful
    /// for the Forge operating team to debug settings that to do do not go in-sync
    /// without having to attach to the database.
    pub configs_synced: SyncState,
}

impl InstanceNetworkStatus {
    /// Derives an Instances network status from the users desired config
    /// and status that we observed from the networking subsystem.
    ///
    /// This mechanism guarantees that the status we return to the user always
    /// matches the latest `Config` set by the user. We can not directly
    /// forwarding the last observed status without taking `Config` into account,
    /// because the observation might have been related to a different config,
    /// and the interfaces therefore won't match.
    pub fn from_config_and_observations(
        dpu_id_to_device_map: HashMap<String, Vec<DpuMachineId>>,
        config: Versioned<&InstanceNetworkConfig>,
        observations: &HashMap<DpuMachineId, InstanceNetworkStatusObservation>,
        is_network_config_request_pending: bool,
    ) -> Self {
        if is_network_config_request_pending {
            return Self::unsynchronized_for_config(&config);
        }

        if observations
            .iter()
            .any(|obs| obs.1.config_version != config.version)
        {
            return Self::unsynchronized_for_config(&config);
        }

        // Observations without interfaces are from unused DPUs.  filter them out
        let observations: HashMap<&DpuMachineId, &InstanceNetworkStatusObservation> = observations
            .iter()
            .filter(|obs| !obs.1.interfaces.is_empty())
            .collect();

        if observations.is_empty() {
            if config.is_host_inband() {
                return Self::synchronized_from_host_interfaces(config.value.interfaces.clone());
            } else {
                return Self::unsynchronized_for_config(&config);
            }
        }

        let mut configs_synced = SyncState::Synced;
        let mut missing_dpus = Vec::default();
        let mut interfaces = Vec::default();
        for config_iface in &config.interfaces {
            let device_locator = config_iface.device_locator.as_ref();

            let dpu_machine_id = device_locator.and_then(|dl| {
                dpu_id_to_device_map
                    .get(&dl.device)
                    .and_then(|id_vec| id_vec.get(dl.device_instance))
            });
            match dpu_machine_id {
                Some(dpu_machine_id) => match observations.get(dpu_machine_id) {
                    Some(dpu_obs) => {
                        let obs_iface = dpu_obs
                            .interfaces
                            .iter()
                            .find(|obs_iface| obs_iface.function_id == config_iface.function_id);

                        match obs_iface {
                            Some(obs_iface) => {
                                interfaces.push(InstanceInterfaceStatus {
                                    function_id: config_iface.function_id.clone(),
                                    mac_address: obs_iface.mac_address.map(Into::into),
                                    addresses: obs_iface.addresses.clone(),
                                    prefixes: obs_iface.prefixes.clone(),
                                    gateways: obs_iface.gateways.clone(),
                                    vpc_id: config_iface.vpc_id,
                                    resolved_vpc_prefixes: config_iface.resolved_vpc_prefixes(),
                                    device: config_iface
                                        .device_locator
                                        .as_ref()
                                        .map(|dl| dl.device.clone()),
                                    device_instance: config_iface
                                        .device_locator
                                        .as_ref()
                                        .map(|dl| dl.device_instance)
                                        .unwrap_or_default(),
                                });
                            }
                            None => {
                                tracing::error!(
                                    dpu_machine_id = ?dpu_machine_id, function_id = ?config_iface.function_id, ?config, ?observations,
                                    "Could not find matching status for interface",
                                );

                                // TODO: Might also be worthwhile to return an error?
                                // On the other hand the error is also visible via returning no IPs - and at least we don't break
                                // all other interfaces this way
                                // UPDATE:  added pending status.
                                interfaces.push(InstanceInterfaceStatus {
                                    function_id: config_iface.function_id.clone(),
                                    mac_address: None,
                                    addresses: Vec::new(),
                                    prefixes: Vec::new(),
                                    gateways: Vec::new(),
                                    vpc_id: config_iface.vpc_id,
                                    resolved_vpc_prefixes: config_iface.resolved_vpc_prefixes(),
                                    device: config_iface
                                        .device_locator
                                        .as_ref()
                                        .map(|dl| dl.device.clone()),
                                    device_instance: config_iface
                                        .device_locator
                                        .as_ref()
                                        .map(|dl| dl.device_instance)
                                        .unwrap_or_default(),
                                });
                                configs_synced = SyncState::Pending;
                            }
                        }
                    }
                    None => {
                        interfaces.push(InstanceInterfaceStatus {
                            function_id: config_iface.function_id.clone(),
                            mac_address: None,
                            addresses: Vec::new(),
                            prefixes: Vec::new(),
                            gateways: Vec::new(),
                            vpc_id: config_iface.vpc_id,
                            resolved_vpc_prefixes: config_iface.resolved_vpc_prefixes(),
                            device: config_iface
                                .device_locator
                                .as_ref()
                                .map(|dl| dl.device.clone()),
                            device_instance: config_iface
                                .device_locator
                                .as_ref()
                                .map(|dl| dl.device_instance)
                                .unwrap_or_default(),
                        });
                        missing_dpus.push(dpu_machine_id);
                        configs_synced = SyncState::Pending;
                    }
                },
                None => {
                    if config
                        .interfaces
                        .iter()
                        .filter(|iface| iface.function_id == InterfaceFunctionId::Physical {})
                        .count()
                        > 1
                    {
                        tracing::error!(
                            ?config,
                            "Found multiple physical interfaces when no device specified",
                        );
                        return Self::unsynchronized_for_config(&config);
                    }

                    if observations.is_empty() {
                        return Self::unsynchronized_for_config(&config);
                    }

                    if let Some((_id, dpu_obs)) = observations.iter().next() {
                        let intf_obs = dpu_obs
                            .interfaces
                            .iter()
                            .find(|iface| iface.function_id == config_iface.function_id);
                        match intf_obs {
                            Some(intf_obs) => {
                                interfaces.push(InstanceInterfaceStatus {
                                    function_id: config_iface.function_id.clone(),
                                    mac_address: intf_obs.mac_address.map(Into::into),
                                    addresses: intf_obs.addresses.clone(),
                                    prefixes: intf_obs.prefixes.clone(),
                                    gateways: intf_obs.gateways.clone(),
                                    vpc_id: config_iface.vpc_id,
                                    resolved_vpc_prefixes: config_iface.resolved_vpc_prefixes(),
                                    device: config_iface
                                        .device_locator
                                        .as_ref()
                                        .map(|dl| dl.device.clone()),
                                    device_instance: config_iface
                                        .device_locator
                                        .as_ref()
                                        .map(|dl| dl.device_instance)
                                        .unwrap_or_default(),
                                });
                            }
                            None => {
                                tracing::error!(
                                    function_id = ?config_iface.function_id, ?config, ?observations,
                                    "Could not find matching status for interface for legacy config",
                                );

                                // TODO: Might also be worthwhile to return an error?
                                // On the other hand the error is also visible via returning no IPs - and at least we don't break
                                // all other interfaces this way
                                interfaces.push(InstanceInterfaceStatus {
                                    function_id: config_iface.function_id.clone(),
                                    mac_address: None,
                                    addresses: Vec::new(),
                                    prefixes: Vec::new(),
                                    gateways: Vec::new(),
                                    vpc_id: config_iface.vpc_id,
                                    resolved_vpc_prefixes: config_iface.resolved_vpc_prefixes(),
                                    device: config_iface
                                        .device_locator
                                        .as_ref()
                                        .map(|dl| dl.device.clone()),
                                    device_instance: config_iface
                                        .device_locator
                                        .as_ref()
                                        .map(|dl| dl.device_instance)
                                        .unwrap_or_default(),
                                });
                            }
                        }
                    }
                }
            }
        }

        if !missing_dpus.is_empty() {
            tracing::info!(
                missing_dpu_ids = ?missing_dpus,
                "Missing observations for DPUs",
            );
        }

        Self {
            interfaces,
            configs_synced,
        }
    }

    /// Creates a `InstanceNetworkStatus` report for cases there the configuration
    /// has not been synchronized.
    ///
    /// This status report will contain an interface for each requested interface,
    /// but all interfaces will have no addresses assigned to them.
    fn unsynchronized_for_config(config: &InstanceNetworkConfig) -> Self {
        Self {
            interfaces: config
                .interfaces
                .iter()
                .map(|iface| InstanceInterfaceStatus {
                    function_id: iface.function_id.clone(),
                    mac_address: None,
                    addresses: Vec::new(),
                    prefixes: Vec::new(),
                    gateways: Vec::new(),
                    vpc_id: iface.vpc_id,
                    resolved_vpc_prefixes: iface.resolved_vpc_prefixes(),
                    device: iface.device_locator.as_ref().map(|dl| dl.device.clone()),
                    device_instance: iface
                        .device_locator
                        .as_ref()
                        .map(|dl| dl.device_instance)
                        .unwrap_or_default(),
                })
                .collect(),
            configs_synced: SyncState::Pending,
        }
    }

    /// Creates an `InstanceNetworkStatus` report for cases where all interfaces on the instance are
    /// host-inband (and we do not expect any observations.)
    fn synchronized_from_host_interfaces(interfaces: Vec<InstanceInterfaceConfig>) -> Self {
        Self {
            interfaces: interfaces
                .into_iter()
                .map(InstanceInterfaceStatus::from_host_inband_interface)
                .collect(),
            configs_synced: SyncState::Synced,
        }
    }
}

/// The actual status of a single network interface of an instance
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceInterfaceStatus {
    /// The function ID that is assigned to this interface
    pub function_id: InterfaceFunctionId,

    /// The MAC address which has been assigned to this interface
    /// The list will be empty if interface configuration hasn't been completed
    /// and therefore the address is unknown.
    pub mac_address: Option<MacAddress>,

    /// The IP addresses reported for this interface, ordered IPv4 before IPv6.
    /// This list is independent from `prefixes` and is empty when no address
    /// is available.
    pub addresses: Vec<IpAddr>,

    /// The prefixes reported for this interface in CIDR notation, ordered IPv4
    /// before IPv6. Prefix lengths follow the selected network and allocation
    /// policy. This list is independent from `addresses`: SLAAC reports an IPv6
    /// prefix without a fixed host address. Consumers must match values by
    /// address family rather than list position. The list is empty when no
    /// prefix is available.
    pub prefixes: Vec<IpNetwork>,

    /// The explicitly configured gateways, in CIDR notation. There is at most
    /// one gateway per address family, associated with the same-family address
    /// and prefix. A family without an explicit gateway is omitted, so this
    /// list can be shorter than `addresses` and is not positionally aligned.
    /// IPv4 precedes IPv6 when both gateways are explicitly configured.
    pub gateways: Vec<IpNetwork>,

    /// The logical VPC this interface belongs to.
    pub vpc_id: Option<VpcId>,

    /// VPC prefixes resolved for this interface, keyed by address family.
    pub resolved_vpc_prefixes: Option<InstanceInterfaceResolvedVpcPrefixes>,

    pub device: Option<String>,
    pub device_instance: usize,
}

impl InstanceInterfaceStatus {
    /// Create a "synthetic" InstanceInterfaceStatus using an InstanceInterfaceConfig as a seed.
    /// Host-inband interfaces do not get real network status observations, so we construct status
    /// ourselves from the host interface's config.
    pub fn from_host_inband_interface(value: InstanceInterfaceConfig) -> Self {
        let resolved_vpc_prefixes = value.resolved_vpc_prefixes();
        let mut address_entries = value.ip_addrs.into_iter().collect::<Vec<_>>();
        address_entries.sort_by_key(|(_, address)| (address.is_ipv6(), *address));

        // Interface prefixes were added after the original host-inband status
        // path. Fall back to the segment gateway's prefix for legacy IPv4
        // configs that do not contain the newer per-interface value.
        let prefixes = address_entries
            .iter()
            .map(|(id, _)| {
                value.interface_prefixes.get(id).copied().or_else(|| {
                    value.network_segment_gateways.get(id).map(|gateway| {
                        // Unwrap safety: the prefix length comes from an
                        // already validated IpNetwork.
                        IpNetwork::new(gateway.network(), gateway.prefix()).unwrap()
                    })
                })
                .or_else(|| {
                    tracing::warn!(
                        network_prefix_id = %id,
                        "Missing prefix in InstanceInterfaceConfig; prefixes field will be empty",
                    );
                    None
                })
            })
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();

        // Gateways are optional per family (IPv6 normally learns one through
        // Router Advertisements), so retain only the explicitly configured
        // values while preserving family order.
        let gateways = address_entries
            .iter()
            .filter_map(|(id, _)| value.network_segment_gateways.get(id).copied())
            .collect();
        let addresses = address_entries
            .into_iter()
            .map(|(_, address)| address)
            .collect();

        Self {
            function_id: value.function_id,
            mac_address: value.host_inband_mac_address,
            addresses,
            prefixes,
            gateways,
            vpc_id: value.vpc_id,
            resolved_vpc_prefixes,
            device: None,
            device_instance: 0,
        }
    }
}

/// The network status that was last reported by the networking subsystem
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceNetworkStatusObservation {
    /// The version of the config that is applied on the networking subsystem
    /// Only if the version is equivalent to the latest desired version we
    /// can actually interpret the results. If the version is outdated, then the
    /// list of interfaces might actually relate to a different interfaces than
    /// the ones that are currently required by the networking config.
    pub config_version: ConfigVersion,

    /// Observed status of the instance config version
    #[serde(default)]
    pub instance_config_version: Option<ConfigVersion>,

    /// Observed status for each configured interface
    #[serde(default)]
    pub interfaces: Vec<InstanceInterfaceStatusObservation>,

    /// When this status was observed
    pub observed_at: DateTime<Utc>,
}

impl InstanceNetworkStatusObservation {
    pub fn any_observed_version_changed(&self, other: &Self) -> bool {
        self.config_version != other.config_version
            || self.instance_config_version != other.instance_config_version
    }

    pub fn aggregate_instance_observation(
        dpu_snapshots: &[Machine],
    ) -> HashMap<DpuMachineId, InstanceNetworkStatusObservation> {
        let mut observation_map = HashMap::default();

        for dpu_snapshot in dpu_snapshots {
            let Ok(dpu_machine_id) = dpu_snapshot.dpu_machine_id() else {
                continue;
            };
            if let Some(obs) = dpu_snapshot
                .network_status_observation
                .as_ref()
                .and_then(|x| x.instance_network_observation.as_ref())
                .map(|m| InstanceNetworkStatusObservation {
                    config_version: m.config_version,
                    instance_config_version: m.instance_config_version,
                    interfaces: m.interfaces.clone(),
                    observed_at: m.observed_at,
                })
            {
                observation_map.insert(dpu_machine_id, obs);
            }
        }

        observation_map
    }
}

/// The actual status of a single network interface of an instance
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInterfaceStatusObservation {
    /// The function ID that is assigned to this interface
    pub function_id: InterfaceFunctionId,

    /// The MAC address which has been assigned to this interface
    /// The list will be empty if interface configuration hasn't been completed
    /// and therefore the address is unknown.
    #[serde(default)]
    pub mac_address: Option<SerializableMacAddress>,

    /// The IP addresses reported for this interface, ordered IPv4 before IPv6.
    /// This list is independent from `prefixes` and is empty when no address
    /// is available.
    #[serde(default)]
    pub addresses: Vec<IpAddr>,

    /// The prefixes reported for this interface in CIDR notation, ordered IPv4
    /// before IPv6. Prefix lengths follow the selected network and allocation
    /// policy. This list is independent from `addresses`: SLAAC reports an IPv6
    /// prefix without a fixed host address. Consumers must match values by
    /// address family rather than list position. The list is empty when no
    /// prefix is available.
    #[serde(default)]
    pub prefixes: Vec<IpNetwork>,

    /// The explicitly configured gateways, in CIDR notation. There is at most
    /// one gateway per address family, associated with the same-family address
    /// and prefix. A family without an explicit gateway is omitted, so this
    /// list can be shorter than `addresses` and is not positionally aligned.
    /// IPv4 precedes IPv6 when both gateways are explicitly configured.
    #[serde(default)]
    pub gateways: Vec<IpNetwork>,

    /// The details of the network security that has
    /// actually been applied to the interface.
    pub network_security_group: Option<NetworkSecurityGroupStatusObservation>,

    /// An ID used to associated the interface status with the interface config.
    #[serde(default)]
    pub internal_uuid: Option<uuid::Uuid>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fmt::Write;
    use std::str::FromStr;

    use carbide_uuid::machine::DpuMachineId as MachineId;
    use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
    use carbide_uuid::vpc::VpcPrefixId;

    use super::*;
    use crate::instance::config::network::{
        InstanceInterfaceConfig, InstanceInterfaceIpFamilyMode, InstanceInterfaceVpcSelection,
        Ipv6InterfaceConfig, NetworkDetails,
    };
    use crate::network_security_group::NetworkSecurityGroupSource;

    #[test]
    fn deserialize_old_network_status_observation() {
        let timestamp: DateTime<Utc> = Utc::now();
        let serialized_timestamp = format!("{timestamp:?}");
        let version = ConfigVersion::initial();

        let observation = InstanceNetworkStatusObservation {
            instance_config_version: None,
            config_version: version,
            interfaces: Vec::new(),
            observed_at: timestamp,
        };

        // Let's make sure the one without the instance_config_version
        // doesn't cause an issue.
        let serialized = format!(
            "{{\"config_version\":\"{version}\",\"interfaces\":[],\"observed_at\":\"{serialized_timestamp}\"}}"
        );

        assert_eq!(
            serde_json::from_str::<InstanceNetworkStatusObservation>(&serialized).unwrap(),
            observation
        );
    }

    #[test]
    fn serialize_network_status_observation() {
        let timestamp: DateTime<Utc> = Utc::now();
        let serialized_timestamp = format!("{timestamp:?}");
        let version = ConfigVersion::initial();
        let instance_version = version;

        let mut observation = InstanceNetworkStatusObservation {
            instance_config_version: Some(instance_version),
            config_version: version,
            interfaces: Vec::new(),
            observed_at: timestamp,
        };
        let serialized = serde_json::to_string(&observation).unwrap();
        assert_eq!(
            serialized,
            format!(
                r#"{{"config_version":"{}","instance_config_version":"{}","interfaces":[],"observed_at":"{}"}}"#,
                instance_version.version_string(),
                version.version_string(),
                serialized_timestamp
            )
        );
        assert_eq!(
            serde_json::from_str::<InstanceNetworkStatusObservation>(&serialized).unwrap(),
            observation
        );

        observation
            .interfaces
            .push(InstanceInterfaceStatusObservation {
                function_id: InterfaceFunctionId::Physical {},
                mac_address: None,
                addresses: Vec::new(),
                prefixes: Vec::new(),
                gateways: Vec::new(),
                network_security_group: None,
                internal_uuid: None,
            });
        observation
            .interfaces
            .push(InstanceInterfaceStatusObservation {
                function_id: InterfaceFunctionId::Virtual { id: 1 },
                mac_address: Some(MacAddress::new([1, 2, 3, 4, 5, 6]).into()),
                addresses: vec!["127.1.2.3".parse().unwrap()],
                prefixes: vec!["127.1.2.3/32".parse().unwrap()],
                gateways: vec!["127.1.2.1".parse().unwrap()],
                network_security_group: Some(NetworkSecurityGroupStatusObservation {
                    id: "c7c056c8-daa5-11ef-b221-c76a97b6c2ec".parse().unwrap(),
                    source: NetworkSecurityGroupSource::Instance,
                    version: "V1-T1".parse().unwrap(),
                }),
                internal_uuid: None,
            });
        let serialized = serde_json::to_string(&observation).unwrap();
        let mut expected = format!(
            r#"{{"config_version":"{}","instance_config_version":"{}","interfaces":["#,
            instance_version.version_string(),
            version.version_string()
        );
        write!(
            &mut expected,
            r#"{{"function_id":{{"type":"physical"}},"mac_address":null,"addresses":[],"prefixes":[],"gateways":[],"network_security_group":null,"internal_uuid":null}},"#
        )
        .unwrap();
        write!(&mut expected, r#"{{"function_id":{{"type":"virtual","id":1}},"mac_address":"01:02:03:04:05:06","addresses":["127.1.2.3"],"prefixes":["127.1.2.3/32"],"gateways":["127.1.2.1/32"],"network_security_group":{{"id":"c7c056c8-daa5-11ef-b221-c76a97b6c2ec","version":"V1-T1","source":"INSTANCE"}},"internal_uuid":null}}"#).unwrap();
        write!(
            &mut expected,
            r#"],"observed_at":"{serialized_timestamp}"}}"#
        )
        .unwrap();
        assert_eq!(serialized, expected);
        assert_eq!(
            serde_json::from_str::<InstanceNetworkStatusObservation>(&serialized).unwrap(),
            observation
        );
    }

    fn network_config() -> InstanceNetworkConfig {
        let base_uuid: NetworkSegmentId =
            uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1200").into();
        let prefix_uuid: NetworkPrefixId =
            uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1400").into();

        InstanceNetworkConfig {
            interfaces: vec![
                InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Physical {},
                    network_segment_id: Some(base_uuid),
                    ip_addrs: HashMap::from([(prefix_uuid, "127.0.0.1".parse().unwrap())]),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::from([(
                        prefix_uuid,
                        "127.0.0.1/32".parse().unwrap(),
                    )]),
                    network_segment_gateways: HashMap::from([(
                        prefix_uuid,
                        "127.0.0.1/32".parse().unwrap(),
                    )]),
                    host_inband_mac_address: None,
                    network_details: None,
                    vpc_selection: None,
                    device_locator: None,
                    internal_uuid: uuid::Uuid::new_v4(),
                    vpc_id: None,
                },
                InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Virtual { id: 1 },
                    network_segment_id: Some(base_uuid.offset(1)),
                    ip_addrs: HashMap::from([(
                        prefix_uuid.offset(1),
                        "127.0.0.2".parse().unwrap(),
                    )]),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::from([(
                        prefix_uuid.offset(1),
                        "127.0.0.2/32".parse().unwrap(),
                    )]),
                    network_segment_gateways: HashMap::from([(
                        prefix_uuid.offset(1),
                        "127.0.0.2/32".parse().unwrap(),
                    )]),
                    host_inband_mac_address: None,
                    network_details: None,
                    vpc_selection: None,
                    device_locator: None,
                    internal_uuid: uuid::Uuid::new_v4(),
                    vpc_id: None,
                },
                InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Virtual { id: 2 },
                    network_segment_id: Some(base_uuid.offset(2)),
                    ip_addrs: HashMap::from([(
                        prefix_uuid.offset(2),
                        "127.0.0.3".parse().unwrap(),
                    )]),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::from([(
                        prefix_uuid.offset(2),
                        "127.0.0.3/32".parse().unwrap(),
                    )]),
                    network_segment_gateways: HashMap::from([(
                        prefix_uuid.offset(2),
                        "127.0.0.3/32".parse().unwrap(),
                    )]),
                    host_inband_mac_address: None,
                    network_details: None,
                    vpc_selection: None,
                    device_locator: None,
                    internal_uuid: uuid::Uuid::new_v4(),
                    vpc_id: None,
                },
            ],
            auto_config: None,
        }
    }

    fn host_inband_network_config() -> InstanceNetworkConfig {
        let base_uuid: NetworkSegmentId =
            uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1200").into();
        let prefix_uuid: NetworkPrefixId =
            uuid::uuid!("91609f10-c91d-470d-a260-6293ea0c1400").into();
        let internal_uuid1 = uuid::Uuid::new_v4();
        let internal_uuid2 = uuid::Uuid::new_v4();
        let internal_uuid3 = uuid::Uuid::new_v4();

        InstanceNetworkConfig {
            interfaces: vec![
                InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Physical {},
                    network_segment_id: Some(base_uuid),
                    ip_addrs: HashMap::from([(prefix_uuid, "127.0.1.2".parse().unwrap())]),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::from([(
                        prefix_uuid,
                        "127.0.1.0/24".parse().unwrap(),
                    )]),
                    network_segment_gateways: HashMap::from([(
                        prefix_uuid,
                        "127.0.1.1/24".parse().unwrap(),
                    )]),
                    host_inband_mac_address: Some(MacAddress::new([1, 2, 3, 4, 5, 6])),
                    network_details: None,
                    vpc_selection: None,
                    device_locator: None,
                    internal_uuid: internal_uuid1,
                    vpc_id: None,
                },
                InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Virtual { id: 1 },
                    network_segment_id: Some(base_uuid.offset(1)),
                    ip_addrs: HashMap::from([(
                        prefix_uuid.offset(1),
                        "127.0.2.2".parse().unwrap(),
                    )]),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::from([(
                        prefix_uuid.offset(1),
                        "127.0.2.0/24".parse().unwrap(),
                    )]),
                    network_segment_gateways: HashMap::from([(
                        prefix_uuid.offset(1),
                        "127.0.2.1/24".parse().unwrap(),
                    )]),
                    host_inband_mac_address: Some(MacAddress::new([1, 2, 3, 4, 5, 16])),
                    network_details: None,
                    vpc_selection: None,
                    device_locator: None,
                    internal_uuid: internal_uuid2,
                    vpc_id: None,
                },
                InstanceInterfaceConfig {
                    function_id: InterfaceFunctionId::Virtual { id: 2 },
                    network_segment_id: Some(base_uuid.offset(2)),
                    ip_addrs: HashMap::from([(
                        prefix_uuid.offset(2),
                        "127.0.3.2".parse().unwrap(),
                    )]),
                    requested_ip_addr: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                    interface_prefixes: HashMap::from([(
                        prefix_uuid.offset(2),
                        "127.0.3.0/24".parse().unwrap(),
                    )]),
                    network_segment_gateways: HashMap::from([(
                        prefix_uuid.offset(2),
                        "127.0.3.1/24".parse().unwrap(),
                    )]),
                    host_inband_mac_address: Some(MacAddress::new([1, 2, 3, 4, 5, 26])),
                    network_details: None,
                    vpc_selection: None,
                    device_locator: None,
                    internal_uuid: internal_uuid3,
                    vpc_id: None,
                },
            ],
            auto_config: None,
        }
    }

    const DPU_ID1: &str = "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg";

    fn observations_for_config(
        config: &InstanceNetworkConfig,
        config_version: ConfigVersion,
    ) -> HashMap<MachineId, InstanceNetworkStatusObservation> {
        let mut observations = HashMap::default();

        // put the interfaces in a different order so the status are not sequential
        let interfaces = vec![
            &config.interfaces[2],
            &config.interfaces[0],
            &config.interfaces[1],
        ];
        let mut obs = Vec::default();

        for iface in interfaces {
            let mac_address = iface.host_inband_mac_address.map(|mac| mac.into());
            let addresses = iface.ip_addrs.values().copied().collect();
            let prefixes = iface.interface_prefixes.values().copied().collect();
            let gateways = iface.network_segment_gateways.values().copied().collect();

            obs.push(InstanceInterfaceStatusObservation {
                function_id: iface.function_id.clone(),
                mac_address,
                addresses,
                prefixes,
                gateways,
                network_security_group: Some(NetworkSecurityGroupStatusObservation {
                    id: "c7c056c8-daa5-11ef-b221-c76a97b6c2ec".parse().unwrap(),
                    source: NetworkSecurityGroupSource::Instance,
                    version: "V1-T1".parse().unwrap(),
                }),
                internal_uuid: Some(iface.internal_uuid),
            });
        }
        observations.insert(
            MachineId::from_str(DPU_ID1).unwrap(),
            InstanceNetworkStatusObservation {
                instance_config_version: None, // Reported by rpc::DpuNetworkStatus not rpc::InstanceNetworkStatusObservation
                config_version,
                observed_at: Utc::now(),
                interfaces: obs,
            },
        );
        observations
    }

    fn unsynced_status() -> InstanceNetworkStatus {
        InstanceNetworkStatus {
            interfaces: vec![
                InstanceInterfaceStatus {
                    function_id: InterfaceFunctionId::Physical {},
                    mac_address: None,
                    addresses: Vec::new(),
                    prefixes: Vec::new(),
                    gateways: Vec::new(),
                    vpc_id: None,
                    resolved_vpc_prefixes: None,
                    device: None,
                    device_instance: 0,
                },
                InstanceInterfaceStatus {
                    function_id: InterfaceFunctionId::Virtual { id: 1 },
                    mac_address: None,
                    addresses: Vec::new(),
                    prefixes: Vec::new(),
                    gateways: Vec::new(),
                    vpc_id: None,
                    resolved_vpc_prefixes: None,
                    device: None,
                    device_instance: 0,
                },
                InstanceInterfaceStatus {
                    function_id: InterfaceFunctionId::Virtual { id: 2 },
                    mac_address: None,
                    addresses: Vec::new(),
                    prefixes: Vec::new(),
                    gateways: Vec::new(),
                    vpc_id: None,
                    resolved_vpc_prefixes: None,
                    device: None,
                    device_instance: 0,
                },
            ],
            configs_synced: SyncState::Pending,
        }
    }

    fn expected_status(config: &InstanceNetworkConfig) -> InstanceNetworkStatus {
        let mut interface_status = Vec::default();

        let mut iface_iter = config.interfaces.iter();
        let iface = iface_iter.next().unwrap();

        interface_status.push(InstanceInterfaceStatus {
            function_id: InterfaceFunctionId::Physical {},
            mac_address: iface.host_inband_mac_address,
            addresses: iface.ip_addrs.values().copied().collect(),
            prefixes: iface.interface_prefixes.values().copied().collect(),
            gateways: iface.network_segment_gateways.values().copied().collect(),
            vpc_id: iface.vpc_id,
            resolved_vpc_prefixes: iface.resolved_vpc_prefixes(),
            device: iface.device_locator.as_ref().map(|dl| dl.device.clone()),
            device_instance: iface
                .device_locator
                .as_ref()
                .map(|dl| dl.device_instance)
                .unwrap_or_default(),
        });
        let iface = iface_iter.next().unwrap();

        interface_status.push(InstanceInterfaceStatus {
            function_id: InterfaceFunctionId::Virtual { id: 1 },
            mac_address: iface.host_inband_mac_address,
            addresses: iface.ip_addrs.values().copied().collect(),
            prefixes: iface.interface_prefixes.values().copied().collect(),
            gateways: iface.network_segment_gateways.values().copied().collect(),
            vpc_id: iface.vpc_id,
            resolved_vpc_prefixes: iface.resolved_vpc_prefixes(),
            device: iface.device_locator.as_ref().map(|dl| dl.device.clone()),
            device_instance: iface
                .device_locator
                .as_ref()
                .map(|dl| dl.device_instance)
                .unwrap_or_default(),
        });

        let iface = iface_iter.next().unwrap();

        interface_status.push(InstanceInterfaceStatus {
            function_id: InterfaceFunctionId::Virtual { id: 2 },
            mac_address: iface.host_inband_mac_address,
            addresses: iface.ip_addrs.values().copied().collect(),
            prefixes: iface.interface_prefixes.values().copied().collect(),
            gateways: iface.network_segment_gateways.values().copied().collect(),
            vpc_id: iface.vpc_id,
            resolved_vpc_prefixes: iface.resolved_vpc_prefixes(),
            device: iface.device_locator.as_ref().map(|dl| dl.device.clone()),
            device_instance: iface
                .device_locator
                .as_ref()
                .map(|dl| dl.device_instance)
                .unwrap_or_default(),
        });

        InstanceNetworkStatus {
            interfaces: interface_status,
            configs_synced: SyncState::Synced,
        }
    }

    fn expected_host_inband_status() -> InstanceNetworkStatus {
        InstanceNetworkStatus {
            interfaces: vec![
                InstanceInterfaceStatus {
                    function_id: InterfaceFunctionId::Physical {},
                    mac_address: Some(MacAddress::new([1, 2, 3, 4, 5, 6])),
                    addresses: vec!["127.0.1.2".parse().unwrap()],
                    prefixes: vec!["127.0.1.0/24".parse().unwrap()],
                    gateways: vec!["127.0.1.1/24".parse().unwrap()],
                    vpc_id: None,
                    resolved_vpc_prefixes: None,
                    device: None,
                    device_instance: 0,
                },
                InstanceInterfaceStatus {
                    function_id: InterfaceFunctionId::Virtual { id: 1 },
                    mac_address: Some(MacAddress::new([1, 2, 3, 4, 5, 16])),
                    addresses: vec!["127.0.2.2".parse().unwrap()],
                    prefixes: vec!["127.0.2.0/24".parse().unwrap()],
                    gateways: vec!["127.0.2.1/24".parse().unwrap()],
                    vpc_id: None,
                    resolved_vpc_prefixes: None,
                    device: None,
                    device_instance: 0,
                },
                InstanceInterfaceStatus {
                    function_id: InterfaceFunctionId::Virtual { id: 2 },
                    mac_address: Some(MacAddress::new([1, 2, 3, 4, 5, 26])),
                    addresses: vec!["127.0.3.2".parse().unwrap()],
                    prefixes: vec!["127.0.3.0/24".parse().unwrap()],
                    gateways: vec!["127.0.3.1/24".parse().unwrap()],
                    vpc_id: None,
                    resolved_vpc_prefixes: None,
                    device: None,
                    device_instance: 0,
                },
            ],
            configs_synced: SyncState::Synced,
        }
    }

    #[test]
    fn network_status_without_observations() {
        let config = network_config();
        let version = ConfigVersion::initial();

        let status = InstanceNetworkStatus::from_config_and_observations(
            HashMap::default(),
            Versioned::new(&config, version),
            &HashMap::default(),
            false,
        );
        assert_eq!(status, unsynced_status())
    }

    /// Allocation-derived prefix resolution remains visible while observed
    /// interface addresses are still pending synchronization.
    #[test]
    fn network_status_without_observations_includes_resolved_prefixes() {
        let vpc_id = VpcId::new();
        let ipv4_vpc_prefix_id = VpcPrefixId::new();
        let ipv6_vpc_prefix_id = VpcPrefixId::new();
        let mut config = network_config();
        let interface = &mut config.interfaces[0];
        interface.network_details = Some(NetworkDetails::VpcPrefixId(ipv4_vpc_prefix_id));
        interface.vpc_selection = Some(InstanceInterfaceVpcSelection {
            vpc_id,
            family_mode: InstanceInterfaceIpFamilyMode::DualStack,
        });
        interface.ipv6_interface_config = Some(Ipv6InterfaceConfig {
            vpc_prefix_id: ipv6_vpc_prefix_id,
            requested_ip_addr: None,
        });
        interface.vpc_id = Some(vpc_id);

        let status = InstanceNetworkStatus::from_config_and_observations(
            HashMap::default(),
            Versioned::new(&config, ConfigVersion::initial()),
            &HashMap::default(),
            false,
        );

        assert!(status.interfaces[0].addresses.is_empty());
        assert_eq!(
            status.interfaces[0].resolved_vpc_prefixes,
            Some(InstanceInterfaceResolvedVpcPrefixes {
                ipv4_vpc_prefix_id: Some(ipv4_vpc_prefix_id),
                ipv6_vpc_prefix_id: Some(ipv6_vpc_prefix_id),
            })
        );
    }

    #[test]
    fn network_status_with_correct_version_observation() {
        let config = network_config();
        let version = ConfigVersion::initial();
        let observations = observations_for_config(&config, version);

        let status = InstanceNetworkStatus::from_config_and_observations(
            HashMap::default(),
            Versioned::new(&config, version),
            &observations,
            false,
        );
        assert_eq!(status, expected_status(&config))
    }

    #[test]
    fn network_status_with_update_going_on() {
        let config = network_config();
        let version = ConfigVersion::initial();
        let observations = observations_for_config(&config, version);

        let status = InstanceNetworkStatus::from_config_and_observations(
            HashMap::default(),
            Versioned::new(&config, version),
            &observations,
            true,
        );
        assert_eq!(status, unsynced_status())
    }

    #[test]
    fn network_status_with_mismatched_version_observation() {
        let config = network_config();
        let version = ConfigVersion::initial();
        let observations = observations_for_config(&config, version);

        let status = InstanceNetworkStatus::from_config_and_observations(
            HashMap::default(),
            Versioned::new(&config, version.increment()),
            &observations,
            false,
        );
        assert_eq!(status, unsynced_status())
    }

    #[test]
    fn network_status_host_inband_interface_config() {
        let config = host_inband_network_config();
        let version = ConfigVersion::initial();
        let status = InstanceNetworkStatus::from_config_and_observations(
            HashMap::default(),
            Versioned::new(&config, version.increment()),
            // No observations
            &HashMap::default(),
            false,
        );
        assert_eq!(status, expected_host_inband_status())
    }

    #[test]
    fn host_inband_status_orders_dual_stack_fields_by_family() {
        let mut interface = host_inband_network_config().interfaces.remove(0);
        let ipv6_prefix_id = NetworkPrefixId::new();
        interface
            .ip_addrs
            .insert(ipv6_prefix_id, "2001:db8::2".parse().unwrap());
        interface
            .interface_prefixes
            .insert(ipv6_prefix_id, "2001:db8::/64".parse().unwrap());

        let status = InstanceInterfaceStatus::from_host_inband_interface(interface);

        assert_eq!(
            status.addresses,
            vec![
                "127.0.1.2".parse::<IpAddr>().unwrap(),
                "2001:db8::2".parse::<IpAddr>().unwrap(),
            ],
        );
        assert_eq!(
            status.prefixes,
            vec![
                "127.0.1.0/24".parse::<IpNetwork>().unwrap(),
                "2001:db8::/64".parse::<IpNetwork>().unwrap(),
            ],
        );
        assert_eq!(
            status.gateways,
            vec!["127.0.1.1/24".parse::<IpNetwork>().unwrap()]
        );
    }
}
