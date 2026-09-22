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
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use model::instance::status::network::{
    InstanceInterfaceStatusObservation, InstanceNetworkStatusObservation,
};
use model::machine::network::{
    DpuFabricInterfaceStatusObservation, DpuLinkStatusObservation, MachineNetworkStatusObservation,
    ManagedHostQuarantineMode, ManagedHostQuarantineState,
};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl From<rpc::FabricInterfaceData> for DpuFabricInterfaceStatusObservation {
    fn from(interface: rpc::FabricInterfaceData) -> Self {
        Self {
            interface_name: interface.interface_name,
            link_data: interface.link_data.map(Into::into),
        }
    }
}

impl From<rpc::LinkData> for DpuLinkStatusObservation {
    fn from(link: rpc::LinkData) -> Self {
        Self {
            link_type: link.link_type,
            state: link.state,
            carrier_up: link.carrier_up,
            mtu: link.mtu,
            carrier_up_count: link.carrier_up_count,
            carrier_down_count: link.carrier_down_count,
        }
    }
}

impl From<DpuFabricInterfaceStatusObservation> for rpc::FabricInterfaceData {
    fn from(interface: DpuFabricInterfaceStatusObservation) -> Self {
        Self {
            interface_name: interface.interface_name,
            link_data: interface.link_data.map(Into::into),
        }
    }
}

impl From<DpuLinkStatusObservation> for rpc::LinkData {
    fn from(link: DpuLinkStatusObservation) -> Self {
        Self {
            link_type: link.link_type,
            state: link.state,
            carrier_up: link.carrier_up,
            mtu: link.mtu,
            carrier_up_count: link.carrier_up_count,
            carrier_down_count: link.carrier_down_count,
        }
    }
}

impl TryFrom<rpc::DpuNetworkStatus> for MachineNetworkStatusObservation {
    type Error = RpcDataConversionError;

    fn try_from(obs: rpc::DpuNetworkStatus) -> Result<Self, Self::Error> {
        let observed_at = match obs.observed_at {
            Some(timestamp) => {
                let system_time = SystemTime::try_from(timestamp)
                    .map_err(|_| Self::Error::InvalidTimestamp(timestamp.to_string()))?;
                DateTime::from(system_time)
            }
            None => Utc::now(),
        };

        // We're going to piggy-back on InstanceNetworkStatusObservation
        // to get the instance_config_version for now.
        let instance_config_version = match obs.instance_config_version {
            Some(version_string) => match version_string.as_str().parse() {
                Ok(version) => Some(version),
                _ => {
                    return Err(RpcDataConversionError::InvalidConfigVersion(format!(
                        "applied_config.instance_config_version: {version_string}"
                    )));
                }
            },
            _ => None,
        };

        let instance_network_observation =
            if let Some(version_string) = obs.instance_network_config_version {
                let Ok(version) = version_string.as_str().parse() else {
                    return Err(RpcDataConversionError::InvalidConfigVersion(format!(
                        "applied_config.instance_network_config_version: {version_string}"
                    )));
                };
                let mut interfaces: Vec<InstanceInterfaceStatusObservation> = vec![];
                for iface in obs.interfaces {
                    let v = iface.try_into()?;
                    interfaces.push(v);
                }

                Some(InstanceNetworkStatusObservation {
                    config_version: version,
                    instance_config_version,
                    observed_at,
                    interfaces,
                })
            } else {
                None
            };

        Ok(MachineNetworkStatusObservation {
            observed_at,
            machine_id: obs
                .dpu_machine_id
                .ok_or(Self::Error::MissingArgument("dpu_machine_id"))?,
            agent_version: obs.dpu_agent_version.clone(),
            network_config_version: obs.network_config_version.and_then(|n| n.parse().ok()),
            client_certificate_expiry: obs.client_certificate_expiry_unix_epoch_secs,
            agent_version_superseded_at: None,
            instance_network_observation,
            fabric_interfaces: obs.fabric_interfaces.into_iter().map(Into::into).collect(),
        })
    }
}

// TODO: This API is only used by the carbide-web generating the Network Status page
// It improperly returns the values of a lot of things - since those are not actually
// persisted.
// It would be preferable to migrate carbide-web from reading the status to using
// a better supported API. E.g. the FindMachinesByIds one.
impl From<MachineNetworkStatusObservation> for rpc::DpuNetworkStatus {
    fn from(m: MachineNetworkStatusObservation) -> rpc::DpuNetworkStatus {
        rpc::DpuNetworkStatus {
            dpu_machine_id: Some(m.machine_id),
            dpu_agent_version: m.agent_version.clone(),
            observed_at: Some(m.observed_at.into()),
            network_config_version: m.network_config_version.map(|v| v.version_string()),
            instance_id: None,
            instance_config_version: None,
            instance_network_config_version: None,
            interfaces: vec![],
            network_config_error: None,
            client_certificate_expiry_unix_epoch_secs: None,
            dpu_health: None,
            fabric_interfaces: m.fabric_interfaces.into_iter().map(Into::into).collect(),
            last_dhcp_requests: vec![],
            dpu_extension_service_version: None,
            dpu_extension_services: vec![],
            astra_config_status: None,
        }
    }
}

impl From<ManagedHostQuarantineState> for rpc::ManagedHostQuarantineState {
    fn from(m: ManagedHostQuarantineState) -> Self {
        Self {
            mode: rpc::ManagedHostQuarantineMode::from(m.mode) as i32,
            reason: m.reason,
        }
    }
}

impl From<ManagedHostQuarantineMode> for rpc::ManagedHostQuarantineMode {
    fn from(m: ManagedHostQuarantineMode) -> Self {
        match m {
            ManagedHostQuarantineMode::BlockAllTraffic => {
                rpc::ManagedHostQuarantineMode::BlockAllTraffic
            }
        }
    }
}

impl TryFrom<rpc::ManagedHostQuarantineState> for ManagedHostQuarantineState {
    type Error = RpcDataConversionError;
    fn try_from(value: rpc::ManagedHostQuarantineState) -> Result<Self, Self::Error> {
        Ok(Self {
            reason: value.reason,
            mode: rpc::ManagedHostQuarantineMode::try_from(value.mode)
                .map_err(|_| {
                    RpcDataConversionError::InvalidValue(value.mode.to_string(), "mode".to_string())
                })?
                .into(),
        })
    }
}

impl From<rpc::ManagedHostQuarantineMode> for ManagedHostQuarantineMode {
    fn from(m: rpc::ManagedHostQuarantineMode) -> Self {
        match m {
            rpc::ManagedHostQuarantineMode::BlockAllTraffic => Self::BlockAllTraffic,
        }
    }
}
