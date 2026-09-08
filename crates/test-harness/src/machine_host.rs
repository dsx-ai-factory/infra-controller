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
use carbide_uuid::machine::{HostMachineId, MachineId};
use mac_address::MacAddress;
use model::hardware_info::HardwareInfo;
use model::machine::machine_id::from_hardware_info;
use model::test_support::ManagedHostConfig;

use crate::machine::{TestMachine, TestMachinePrivate};
use crate::network::segment::TestNetworkSegment;
use crate::rpc::forge::forge_server::Forge;
use crate::rpc::forge::{
    DhcpDiscovery, DhcpRecord, ForgeAgentControlRequest, ForgeAgentControlResponse,
    MachineRebootCompletedRequest,
};

#[derive(Clone)]
pub struct TestHostMachine {
    pub id: HostMachineId,
    pub bmc_mac: MacAddress,
    pub api: Arc<Api>,
    hardware_info: HardwareInfo,
    primary_mac: MacAddress,
}

impl TestHostMachine {
    pub(crate) fn new(id: HostMachineId, api: Arc<Api>, config: &ManagedHostConfig) -> Self {
        Self {
            id,
            bmc_mac: config.bmc_mac_address,
            api,
            hardware_info: HardwareInfo::from(config),
            primary_mac: config.dhcp_mac_address(),
        }
    }

    pub fn hardware_info(&self) -> HardwareInfo {
        self.hardware_info.clone()
    }

    pub fn serial(&self) -> Option<String> {
        let hardware_info = self.hardware_info();
        let dmi = hardware_info.dmi_data?;
        if !dmi.product_serial.is_empty() {
            Some(dmi.product_serial)
        } else if !dmi.chassis_serial.is_empty() {
            Some(dmi.chassis_serial)
        } else {
            None
        }
    }

    pub fn primary_mac(&self) -> MacAddress {
        self.primary_mac
    }

    pub async fn dhcp_discover_primary_iface(&self, segment: TestNetworkSegment) -> DhcpRecord {
        self.api
            .discover_dhcp(
                DhcpDiscovery::builder(self.primary_mac(), segment.relay_address)
                    .vendor_string("Bluefield")
                    .tonic_request(),
            )
            .await
            .expect("host primary interface DHCP discovery should succeed")
            .into_inner()
    }

    pub async fn discover_primary_iface(&mut self, segment: TestNetworkSegment) {
        let dhcp_record = self.dhcp_discover_primary_iface(segment).await;
        let hardware_info = self.hardware_info();
        let expected_id =
            from_hardware_info(&hardware_info).expect("host hardware info should yield stable id");
        let discovered_id = self
            .discover_machine(&dhcp_record, hardware_info)
            .await
            .machine_id
            .expect("host discovery should return a machine id");
        assert_eq!(
            discovered_id, expected_id,
            "host discovery should resolve to the stable test machine"
        );
        self.id = discovered_id
            .try_into()
            .expect("host discovery should yield a valid HostMachineId");
    }

    pub async fn reboot_completed(&self) {
        self.api
            .reboot_completed(tonic::Request::new(MachineRebootCompletedRequest {
                machine_id: Some(self.id.into()),
            }))
            .await
            .unwrap();
    }

    pub async fn forge_agent_control(&self) -> ForgeAgentControlResponse {
        self.api
            .forge_agent_control(tonic::Request::new(ForgeAgentControlRequest {
                machine_id: Some(self.id.into()),
            }))
            .await
            .unwrap()
            .into_inner()
    }
}

impl TestMachine for TestHostMachine {
    fn id(&self) -> MachineId {
        self.id.into()
    }

    fn api(&self) -> &Api {
        &self.api
    }
}

impl TestMachinePrivate for TestHostMachine {}
