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
//! Credential-free tenant network observations for the MAT control API.
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use rpc::forge::{InterfaceAddressConfig, ManagedHostNetworkConfigResponse};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub(super) struct TenantNetworkConfig {
    schema_version: u32,
    updated_at: DateTime<Utc>,
    pub(super) dpu_id: MachineId,
    instance_id: InstanceId,
    host_interface_id: Option<String>,
    managed_host_config_version: String,
    instance_network_config_version: String,
    network_virtualization_type: Option<i32>,
    tenant_interfaces: Vec<TenantInterface>,
}

#[derive(Clone, Debug, Serialize)]
struct TenantInterface {
    function_type: i32,
    is_l2_segment: bool,
    vpc_vni: u32,
    addresses: Vec<InterfaceAddressConfig>,
    has_network_security_group: bool,
}

impl TenantNetworkConfig {
    pub(super) fn from_response(
        machine_id: MachineId,
        config: &ManagedHostNetworkConfigResponse,
    ) -> Option<Self> {
        if !config.is_primary_dpu || config.use_admin_network || config.tenant_interfaces.is_empty()
        {
            return None;
        }
        Some(Self {
            schema_version: 1,
            updated_at: Utc::now(),
            dpu_id: machine_id,
            instance_id: config.instance_id?,
            host_interface_id: config.host_interface_id.clone(),
            managed_host_config_version: config.managed_host_config_version.clone(),
            instance_network_config_version: config.instance_network_config_version.clone(),
            network_virtualization_type: config.network_virtualization_type,
            // Whitelist fields: the full response also contains credentials.
            tenant_interfaces: config
                .tenant_interfaces
                .iter()
                .map(|interface| TenantInterface {
                    function_type: interface.function_type,
                    is_l2_segment: interface.is_l2_segment,
                    vpc_vni: interface.vpc_vni,
                    addresses: interface.addresses.clone(),
                    has_network_security_group: interface.network_security_group.is_some(),
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use rpc::forge::{AddressFamily, FlatInterfaceConfig};

    use super::*;

    #[test]
    fn exports_only_active_primary_tenant_network_fields() {
        let dpu_id = "fm100dsq7h9eabr4qrfh88vipv7il5sbpfgfq6lhb30n36offacbmusfb50"
            .parse()
            .unwrap();
        let mut config = ManagedHostNetworkConfigResponse {
            is_primary_dpu: true,
            instance_id: Some("00812118-8c3e-44cb-8dc5-fd9250ddc8f8".parse().unwrap()),
            bgp_leaf_session_password: Some("must-not-export".to_string()),
            tenant_interfaces: vec![FlatInterfaceConfig {
                addresses: vec![InterfaceAddressConfig {
                    address_family: AddressFamily::V6 as i32,
                    ip: "fd00:84:100::1".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let value =
            serde_json::to_value(TenantNetworkConfig::from_response(dpu_id, &config).unwrap())
                .unwrap();
        assert_eq!(
            value["tenant_interfaces"][0]["addresses"][0]["ip"],
            "fd00:84:100::1"
        );
        assert!(!value.to_string().contains("must-not-export"));
        assert!(value["updated_at"].as_str().is_some());
        config.use_admin_network = true;
        assert!(TenantNetworkConfig::from_response(dpu_id, &config).is_none());
        config.use_admin_network = false;
        config.is_primary_dpu = false;
        assert!(TenantNetworkConfig::from_response(dpu_id, &config).is_none());
        config.is_primary_dpu = true;
        config.instance_id = None;
        assert!(TenantNetworkConfig::from_response(dpu_id, &config).is_none());
    }
}
