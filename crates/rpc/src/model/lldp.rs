// SPDX-FileCopyrightText: Copyright (c) 2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use mac_address::MacAddress;
use model::lldp::LldpNeighbor;

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl TryFrom<rpc::forge::InterfaceLldp> for LldpNeighbor {
    type Error = RpcDataConversionError;

    fn try_from(interface: rpc::forge::InterfaceLldp) -> Result<Self, Self::Error> {
        let local_mac_address: MacAddress = interface
            .mac_address
            .parse()
            .map_err(|_| RpcDataConversionError::InvalidMacAddress(interface.mac_address))?;
        let lldp = interface
            .lldp
            .ok_or(RpcDataConversionError::MissingArgument("lldp"))?;
        let med = lldp.med_inventory.unwrap_or_default();

        Ok(Self {
            local_mac_address,
            local_port: lldp.local_port,
            chassis_id_type: lldp.id_type,
            chassis_id_value: lldp.id_value,
            remote_port_type: lldp.remote_port_type,
            remote_port_value: lldp.remote_port_value,
            system_name: lldp.name,
            system_description: lldp.description,
            management_addresses: lldp.ip_address,
            med_serial: med.serial,
            med_manufacturer: med.manufacturer,
            med_model: med.model,
        })
    }
}

impl From<LldpNeighbor> for rpc::forge::InterfaceLldp {
    fn from(neighbor: LldpNeighbor) -> Self {
        let has_med = neighbor.med_serial.is_some()
            || neighbor.med_manufacturer.is_some()
            || neighbor.med_model.is_some();
        let med_inventory = has_med.then_some(rpc::machine_discovery::LldpMedInventory {
            serial: neighbor.med_serial,
            manufacturer: neighbor.med_manufacturer,
            model: neighbor.med_model,
        });

        Self {
            mac_address: neighbor.local_mac_address.to_string(),
            // The deprecated combined `id`/`remote_port` fields are left empty.
            lldp: Some(rpc::machine_discovery::LldpSwitchData {
                name: neighbor.system_name,
                description: neighbor.system_description,
                local_port: neighbor.local_port,
                ip_address: neighbor.management_addresses,
                id_type: neighbor.chassis_id_type,
                id_value: neighbor.chassis_id_value,
                remote_port_type: neighbor.remote_port_type,
                remote_port_value: neighbor.remote_port_value,
                med_inventory,
                ..Default::default()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_neighbor_omits_empty_deprecated_fields() {
        let neighbor = LldpNeighbor {
            local_mac_address: "02:00:00:00:01:c0".parse().unwrap(),
            local_port: "p0".to_string(),
            chassis_id_type: "mac".to_string(),
            chassis_id_value: "02:00:00:00:02:01".to_string(),
            remote_port_type: "ifname".to_string(),
            remote_port_value: "swp1".to_string(),
            system_name: "leaf-sw-01".to_string(),
            system_description: String::new(),
            management_addresses: vec![],
            med_serial: None,
            med_manufacturer: None,
            med_model: None,
        };

        let json = serde_json::to_value(rpc::forge::InterfaceLldp::from(neighbor)).unwrap();
        let lldp = json["lldp"].as_object().unwrap();

        assert!(!lldp.contains_key("id"));
        assert!(!lldp.contains_key("remote_port"));
        assert_eq!(lldp["id_value"], "02:00:00:00:02:01");
        assert_eq!(lldp["remote_port_value"], "swp1");
    }
}
