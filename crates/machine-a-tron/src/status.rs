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
use bmc_mock::HardwareType;
use serde::Serialize;
use ufm_mock::{EpochId, Generation, InventoryId};

use crate::{Guid, InfinibandPortState};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InfinibandPortStatus {
    pub guid: Guid,
    pub state: InfinibandPortState,
}

#[derive(Debug, Clone, Copy, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Machine,
    Dpu,
    Switch,
    PowerShelf,
}

impl From<HardwareType> for DeviceKind {
    fn from(hardware_type: HardwareType) -> Self {
        match hardware_type {
            HardwareType::NvidiaSwitchNd5200Ld | HardwareType::NvidiaSwitchN5700Ld => Self::Switch,
            HardwareType::LiteOnPowerShelf | HardwareType::DeltaPowerShelf => Self::PowerShelf,
            _ => Self::Machine,
        }
    }
}

impl std::fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Machine => f.write_str("machine"),
            Self::Dpu => f.write_str("DPU"),
            Self::Switch => f.write_str("switch"),
            Self::PowerShelf => f.write_str("power shelf"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceStatusConfig {
    pub redfish_reachable_port: u16,
    pub redfish_listen_port: u16,
}

impl DeviceStatusConfig {
    pub fn new(redfish_listen_port: u16) -> Self {
        Self {
            redfish_reachable_port: 443,
            redfish_listen_port,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DevicesStatusResponse {
    pub inventory_id: InventoryId,
    pub epoch_id: EpochId,
    pub generation: Generation,
    #[serde(rename = "machines")]
    pub devices: Vec<DeviceStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceStatus {
    pub mat_id: String,
    pub device_kind: DeviceKind,
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardware_type: Option<HardwareType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mat_state: Option<String>,
    pub api_state: String,
    pub power_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvos_ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub infiniband_ports: Option<Vec<InfinibandPortStatus>>,
    pub bmc: BmcStatus,
    pub dpus: Vec<DeviceStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BmcStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    pub redfish: EndpointStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipmi: Option<EndpointStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh: Option<EndpointStatus>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct EndpointStatus {
    pub reachable_port: u16,
    pub listen_port: u16,
}

impl EndpointStatus {
    pub fn redfish(config: &DeviceStatusConfig) -> Self {
        Self {
            reachable_port: config.redfish_reachable_port,
            listen_port: config.redfish_listen_port,
        }
    }

    pub fn same_port(port: u16) -> Self {
        Self {
            reachable_port: port,
            listen_port: port,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn hardware_types_map_to_device_kinds() {
        check_values(
            [
                Check {
                    scenario: "ND5200_LD is a switch",
                    input: HardwareType::NvidiaSwitchNd5200Ld,
                    expect: DeviceKind::Switch,
                },
                Check {
                    scenario: "N5700_LD is a switch",
                    input: HardwareType::NvidiaSwitchN5700Ld,
                    expect: DeviceKind::Switch,
                },
                Check {
                    scenario: "power shelf is a power shelf",
                    input: HardwareType::LiteOnPowerShelf,
                    expect: DeviceKind::PowerShelf,
                },
                Check {
                    scenario: "server is a machine",
                    input: HardwareType::DellPowerEdgeR750,
                    expect: DeviceKind::Machine,
                },
            ],
            DeviceKind::from,
        );
    }

    #[test]
    fn n5700_ld_status_kind_serializes_as_switch() {
        let kind = DeviceKind::from(HardwareType::NvidiaSwitchN5700Ld);

        assert_eq!(serde_json::to_value(kind).unwrap(), "switch");
    }
}
