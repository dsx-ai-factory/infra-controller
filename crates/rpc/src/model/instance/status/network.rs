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

use std::convert::Into;
use std::net::IpAddr;

use ipnetwork::IpNetwork;
use itertools::Itertools;
use mac_address::MacAddress;
use model::instance::config::network::InterfaceFunctionId;
use model::instance::status::network::{
    InstanceInterfaceStatus, InstanceInterfaceStatusObservation, InstanceNetworkStatus,
};

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl TryFrom<InstanceNetworkStatus> for rpc::InstanceNetworkStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: InstanceNetworkStatus) -> Result<Self, Self::Error> {
        let mut interfaces = Vec::with_capacity(status.interfaces.len());
        for iface in status.interfaces {
            interfaces.push(rpc::InstanceInterfaceStatus::try_from(iface)?);
        }
        Ok(rpc::InstanceNetworkStatus {
            interfaces,
            configs_synced: rpc::SyncState::try_from(status.configs_synced)? as i32,
        })
    }
}

impl TryFrom<InstanceInterfaceStatus> for rpc::InstanceInterfaceStatus {
    type Error = RpcDataConversionError;

    fn try_from(status: InstanceInterfaceStatus) -> Result<Self, Self::Error> {
        Ok(rpc::InstanceInterfaceStatus {
            virtual_function_id: match status.function_id {
                InterfaceFunctionId::Physical {} => None,
                InterfaceFunctionId::Virtual { id } => Some(id as u32),
            },
            mac_address: status.mac_address.map(|mac| mac.to_string()),
            addresses: status
                .addresses
                .into_iter()
                .map(|ip| ip.to_string())
                .collect(),
            prefixes: status
                .prefixes
                .into_iter()
                .map(|ip_network| ip_network.to_string())
                .collect(),
            gateways: status
                .gateways
                .into_iter()
                .map(|ip| ip.to_string())
                .collect(),
            device: status.device,
            device_instance: status.device_instance as u32,
            vpc_id: status.vpc_id,
            resolved_vpc_prefixes: status.resolved_vpc_prefixes.map(|resolved| {
                rpc::forge::InstanceInterfaceResolvedVpcPrefixes {
                    ipv4_vpc_prefix_id: resolved.ipv4_vpc_prefix_id,
                    ipv6_vpc_prefix_id: resolved.ipv6_vpc_prefix_id,
                }
            }),
        })
    }
}

impl TryFrom<rpc::InstanceInterfaceStatusObservation> for InstanceInterfaceStatusObservation {
    type Error = RpcDataConversionError;

    fn try_from(observation: rpc::InstanceInterfaceStatusObservation) -> Result<Self, Self::Error> {
        let function_id = match observation.function_type() {
            rpc::forge::InterfaceFunctionType::Physical => InterfaceFunctionId::Physical {},
            rpc::forge::InterfaceFunctionType::Virtual => {
                InterfaceFunctionId::try_virtual_from(observation.virtual_function_id() as u8)
                    .map_err(|_| {
                        RpcDataConversionError::InvalidVirtualFunctionId(
                            observation.virtual_function_id() as usize,
                        )
                    })?
            }
        };

        let addresses = observation
            .addresses
            .iter()
            .map(|addr| {
                addr.parse::<IpAddr>()
                    .map_err(|_| RpcDataConversionError::InvalidIpAddress(addr.clone()))
            })
            .try_collect()?;

        let gateways = observation
            .gateways
            .iter()
            .map(|gateway| {
                IpNetwork::try_from(gateway.as_str())
                    .map_err(|_| Self::Error::InvalidCidr(gateway.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen_ipv4 = false;
        let mut seen_ipv6 = false;
        for gateway in &gateways {
            let seen = if gateway.is_ipv4() {
                &mut seen_ipv4
            } else {
                &mut seen_ipv6
            };
            if *seen {
                return Err(RpcDataConversionError::InvalidArgument(
                    "gateways must contain at most one entry per address family".to_string(),
                ));
            }
            *seen = true;
        }

        let internal_uuid = if let Some(internal_uuid) = &observation.internal_uuid {
            Some(internal_uuid.try_into().map_err(|_| {
                RpcDataConversionError::InvalidUuid("internal_uuid", internal_uuid.to_string())
            })?)
        } else {
            None
        };

        Ok(Self {
            function_id,
            addresses,
            prefixes: observation
                .prefixes
                .iter()
                .map(|ip_network| {
                    IpNetwork::try_from(ip_network.as_str())
                        .map_err(|_| Self::Error::InvalidCidr(ip_network.to_string()))
                })
                .collect::<Result<Vec<IpNetwork>, Self::Error>>()?,
            gateways,
            mac_address: observation
                .mac_address
                .map(|addr| {
                    addr.parse::<MacAddress>()
                        .map_err(|_| RpcDataConversionError::InvalidMacAddress(addr))
                })
                .transpose()?
                .map(Into::into),
            network_security_group: observation
                .network_security_group
                .map(|nsgo| nsgo.try_into())
                .transpose()?,
            internal_uuid,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;
    use carbide_uuid::vpc::{VpcId, VpcPrefixId};
    use model::instance::config::network::InstanceInterfaceResolvedVpcPrefixes;

    use super::*;

    fn status_observation_with_gateways(
        gateways: Vec<&str>,
    ) -> rpc::InstanceInterfaceStatusObservation {
        rpc::InstanceInterfaceStatusObservation {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            gateways: gateways.into_iter().map(str::to_string).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn status_observation_allows_at_most_one_gateway_per_family() {
        scenarios!(
            run = |gateways| {
                InstanceInterfaceStatusObservation::try_from(
                    status_observation_with_gateways(gateways),
                )
                .map(|_| ())
                .map_err(drop)
            };
            "no gateways" {
                vec![] => Yields(()),
            }
            "one IPv4 gateway" {
                vec!["192.0.2.1/24"] => Yields(()),
            }
            "one IPv6 gateway" {
                vec!["2001:db8::1/64"] => Yields(()),
            }
            "one gateway per family" {
                vec!["192.0.2.1/24", "2001:db8::1/64"] => Yields(()),
            }
            "duplicate IPv4 gateways" {
                vec!["192.0.2.1/24", "198.51.100.1/24"] => Fails,
            }
            "duplicate IPv6 gateways" {
                vec!["2001:db8::1/64", "2001:db8:1::1/64"] => Fails,
            }
        );
    }

    /// Status conversion keeps both family-keyed prefix IDs for a resolved
    /// dual-stack interface in its single logical VPC.
    #[test]
    fn convert_dual_stack_resolved_vpc_prefixes() {
        let vpc_id = VpcId::new();
        let ipv4_vpc_prefix_id = VpcPrefixId::new();
        let ipv6_vpc_prefix_id = VpcPrefixId::new();
        let status = InstanceInterfaceStatus {
            function_id: InterfaceFunctionId::Physical {},
            mac_address: None,
            addresses: Vec::new(),
            prefixes: Vec::new(),
            gateways: Vec::new(),
            vpc_id: Some(vpc_id),
            resolved_vpc_prefixes: Some(InstanceInterfaceResolvedVpcPrefixes {
                ipv4_vpc_prefix_id: Some(ipv4_vpc_prefix_id),
                ipv6_vpc_prefix_id: Some(ipv6_vpc_prefix_id),
            }),
            device: None,
            device_instance: 0,
        };

        let wire = rpc::InstanceInterfaceStatus::try_from(status).unwrap();
        let resolved = wire.resolved_vpc_prefixes.unwrap();

        assert_eq!(wire.vpc_id, Some(vpc_id));
        assert_eq!(resolved.ipv4_vpc_prefix_id, Some(ipv4_vpc_prefix_id));
        assert_eq!(resolved.ipv6_vpc_prefix_id, Some(ipv6_vpc_prefix_id));
    }
}
