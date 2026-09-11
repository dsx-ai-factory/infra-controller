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

use std::sync::Arc;

use carbide_api_core::test_support::Api;
use carbide_network::virtualization::build_dual_stack_list;
use carbide_uuid::machine::{DpuMachineId, MachineId};
use mac_address::MacAddress;
use model::hardware_info::HardwareInfo;
use model::test_support::DpuConfig;

use crate::machine::{TestMachine, TestMachinePrivate};
use crate::network::segment::TestNetworkSegment;
use crate::rpc::forge::forge_server::Forge;
use crate::rpc::forge::{DhcpDiscovery, DhcpRecord, ManagedHostNetworkConfigRequest};

#[derive(Clone)]
pub struct TestDpuMachine {
    pub id: DpuMachineId,
    pub bmc_mac: MacAddress,
    pub api: Arc<Api>,
    hardware_info: HardwareInfo,
    oob_mac: MacAddress,
}

impl TestDpuMachine {
    pub(crate) fn new(id: DpuMachineId, api: Arc<Api>, config: &DpuConfig) -> Self {
        Self {
            id,
            bmc_mac: config.bmc_mac_address,
            api,
            hardware_info: HardwareInfo::from(config),
            oob_mac: config.oob_mac_address,
        }
    }

    pub fn oob_mac(&self) -> MacAddress {
        self.oob_mac
    }

    pub async fn dhcp_discover_oob_iface(&self, segment: TestNetworkSegment) -> DhcpRecord {
        self.api
            .discover_dhcp(
                DhcpDiscovery::builder(self.oob_mac(), segment.relay_address)
                    .vendor_string("SomeVendor")
                    .tonic_request(),
            )
            .await
            .expect("DPU OOB interface DHCP discovery should succeed")
            .into_inner()
    }

    pub async fn discover_oob_iface(&self, segment: TestNetworkSegment) {
        let dhcp_record = self.dhcp_discover_oob_iface(segment).await;
        let discovered_id = self
            .discover_machine(&dhcp_record, self.hardware_info.clone())
            .await
            .machine_id
            .expect("DPU discovery should return a machine id");
        assert_eq!(
            discovered_id,
            self.id.into(),
            "DPU discovery should resolve to the expected test machine"
        );
    }

    /// Simulate forge-dpu-agent fetching, applying, and reporting DPU network status.
    pub async fn record_network_status(&self) {
        record_dpu_network_status(&self.api, self.id).await;
    }
}

impl TestMachine for TestDpuMachine {
    fn id(&self) -> MachineId {
        self.id.into()
    }

    fn api(&self) -> &Api {
        &self.api
    }
}

impl TestMachinePrivate for TestDpuMachine {}

// This harness models the compatibility fields consumed by older agents.
#[allow(deprecated)]
async fn record_dpu_network_status(api: &Api, dpu_machine_id: DpuMachineId) {
    let network_config = api
        .get_managed_host_network_config(tonic::Request::new(ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_machine_id),
        }))
        .await
        .expect("managed host network config should be available")
        .into_inner();

    let instance_network_config_version =
        if network_config.instance_network_config_version.is_empty() {
            None
        } else {
            Some(network_config.instance_network_config_version.clone())
        };
    let instance_config_version = api
        .find_instance_by_machine_id(tonic::Request::new(dpu_machine_id.into()))
        .await
        .expect("instance lookup by machine id should succeed")
        .into_inner()
        .instances
        .pop()
        .map(|instance| {
            if !network_config.use_admin_network {
                assert_eq!(
                    instance_network_config_version
                        .as_ref()
                        .expect("instance network config version should be set")
                        .as_str(),
                    instance.network_config_version,
                    "Different network config versions reported via FindInstanceByMachineId and GetManagedHostNetworkConfig"
                );
            }
            instance.config_version
        });

    let interfaces = if network_config.use_admin_network {
        let iface = network_config
            .admin_interface
            .as_ref()
            .expect("admin interface should be available when using admin network");
        vec![crate::rpc::forge::InstanceInterfaceStatusObservation {
            function_type: iface.function_type,
            virtual_function_id: None,
            mac_address: None,
            addresses: build_dual_stack_list(
                iface.ip.clone(),
                iface.ipv6_interface_config.as_ref().map(|v6| v6.ip.clone()),
            ),
            prefixes: build_dual_stack_list(
                iface.interface_prefix.clone(),
                iface
                    .ipv6_interface_config
                    .as_ref()
                    .map(|v6| v6.interface_prefix.clone()),
            ),
            gateways: build_dual_stack_list(iface.gateway.clone(), None),
            network_security_group: None,
            internal_uuid: iface.internal_uuid.clone(),
        }]
    } else {
        network_config
            .tenant_interfaces
            .iter()
            .map(
                |iface| crate::rpc::forge::InstanceInterfaceStatusObservation {
                    function_type: iface.function_type,
                    virtual_function_id: iface.virtual_function_id,
                    mac_address: None,
                    addresses: build_dual_stack_list(
                        iface.ip.clone(),
                        iface.ipv6_interface_config.as_ref().map(|v6| v6.ip.clone()),
                    ),
                    prefixes: build_dual_stack_list(
                        iface.interface_prefix.clone(),
                        iface
                            .ipv6_interface_config
                            .as_ref()
                            .map(|v6| v6.interface_prefix.clone()),
                    ),
                    gateways: build_dual_stack_list(iface.gateway.clone(), None),
                    network_security_group: None,
                    internal_uuid: iface.internal_uuid.clone(),
                },
            )
            .collect()
    };

    let dpu_extension_services = network_config
        .dpu_extension_services
        .iter()
        .map(
            |extension_service| crate::rpc::forge::DpuExtensionServiceStatusObservation {
                service_id: extension_service.service_id.clone(),
                service_type: extension_service.service_type,
                service_name: String::new(),
                version: extension_service.version.to_string(),
                state: crate::rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning
                    as i32,
                components: vec![],
                message: String::new(),
                removed: extension_service.removed.clone(),
            },
        )
        .collect();

    api.record_dpu_network_status(tonic::Request::new(crate::rpc::forge::DpuNetworkStatus {
        dpu_machine_id: Some(dpu_machine_id),
        dpu_agent_version: Some("test-dpu-agent-version".to_string()),
        observed_at: None,
        dpu_health: Some(crate::rpc::health::HealthReport {
            source: "forge-dpu-agent".to_string(),
            triggered_by: None,
            observed_at: None,
            successes: vec![],
            alerts: vec![],
        }),
        network_config_version: Some(network_config.managed_host_config_version.clone()),
        instance_id: network_config.instance_id,
        instance_config_version,
        instance_network_config_version,
        interfaces,
        network_config_error: None,
        client_certificate_expiry_unix_epoch_secs: None,
        fabric_interfaces: vec![],
        last_dhcp_requests: vec![],
        dpu_extension_service_version: network_config
            .instance
            .map(|instance| instance.dpu_extension_service_version),
        dpu_extension_services,
        astra_config_status: None,
    }))
    .await
    .expect("DPU network status should be recorded");
}
