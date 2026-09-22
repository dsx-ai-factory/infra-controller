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

use itertools::Itertools;
use model::network_devices::{DpuToNetworkDeviceMap, NetworkDevice, NetworkTopologyData};

use crate as rpc;

impl From<NetworkTopologyData> for rpc::forge::NetworkTopologyData {
    fn from(value: NetworkTopologyData) -> Self {
        let mut network_devices = vec![];

        for network_device in value.network_devices {
            let devices = network_device.dpus.into_iter().map_into().collect_vec();

            network_devices.push(rpc::forge::NetworkDevice {
                id: network_device.id,
                name: network_device.name,
                description: network_device.description,
                mgmt_ip: network_device
                    .ip_addresses
                    .iter()
                    .map(|x| x.to_string())
                    .collect_vec(),
                devices,
                discovered_via: network_device.discovered_via.to_string(),
                device_type: network_device.device_type.to_string(),
            });
        }

        rpc::forge::NetworkTopologyData { network_devices }
    }
}

impl From<DpuToNetworkDeviceMap> for rpc::forge::ConnectedDevice {
    fn from(value: DpuToNetworkDeviceMap) -> Self {
        Self {
            id: Some(value.dpu_id.into()),
            local_port: value.local_port.to_string(),
            remote_port: value.remote_port.clone(),
            network_device_id: Some(value.network_device_id),
        }
    }
}

impl From<NetworkDevice> for rpc::forge::NetworkDevice {
    fn from(value: NetworkDevice) -> Self {
        Self {
            id: value.id.clone(),
            name: value.name.clone(),
            description: value.description.clone(),
            mgmt_ip: value.ip_addresses.iter().map(|i| i.to_string()).collect(),
            discovered_via: value.discovered_via.to_string(),
            device_type: value.device_type.to_string(),
            devices: value.dpus.into_iter().map_into().collect(),
        }
    }
}
